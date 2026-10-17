use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use moka::future::Cache;
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, Semaphore};
use tower_governor::{GovernorLayer, governor::GovernorConfigBuilder};

const DINO_POOL: &[&str] = &[
    "Tyrannosaurus", "Triceratops", "Velociraptor", "Stegosaurus", "Spinosaurus",
    "Ankylosaurus", "Brachiosaurus", "Allosaurus", "Parasaurolophus", "Carnotaurus",
    "Therizinosaurus", "Deinonychus", "Pachycephalosaurus", "Diplodocus", "Iguanodon",
    "Giganotosaurus", "Archaeopteryx", "Baryonyx", "Corythosaurus", "Gallimimus",
    "Maiasaura", "Microraptor", "Oviraptor", "Plateosaurus", "Protoceratops",
    "Styracosaurus", "Utahraptor", "Compsognathus", "Dilophosaurus", "Edmontosaurus",
    "Kentrosaurus", "Megalosaurus", "Amargasaurus", "Concavenator", "Dreadnoughtus",
];

#[derive(Clone, Serialize, Deserialize)]
pub struct TriviaFact {
    pub dino: String,
    pub category: String,
    pub fact: String,
    pub wiki_url: String,
}

// --- 24h Zero-Allocation Rolling Telemetry (1440 x 1-minute slots) ---
struct MinuteSlot {
    epoch_min: AtomicU64,
    requests: AtomicU64,
    wiki_calls: AtomicU64,
}

pub struct Stats24h {
    slots: Vec<MinuteSlot>,
    total_facts_harvested: AtomicU64,
}

impl Stats24h {
    pub fn new() -> Self {
        Self {
            slots: (0..1440)
                .map(|_| MinuteSlot {
                    epoch_min: AtomicU64::new(0),
                    requests: AtomicU64::new(0),
                    wiki_calls: AtomicU64::new(0),
                })
                .collect(),
            total_facts_harvested: AtomicU64::new(1),
        }
    }

    fn now_min() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            / 60
    }

    fn current_slot(&self) -> &MinuteSlot {
        let now = Self::now_min();
        let slot = &self.slots[(now % 1440) as usize];
        let prev = slot.epoch_min.load(Ordering::Relaxed);
        if prev != now
            && slot
                .epoch_min
                .compare_exchange(prev, now, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
        {
            slot.requests.store(0, Ordering::Relaxed);
            slot.wiki_calls.store(0, Ordering::Relaxed);
        }
        slot
    }

    pub fn rec_req(&self) {
        self.current_slot().requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn rec_wiki(&self) {
        self.current_slot().wiki_calls.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> (u64, u64, u64, Vec<u64>) {
        let now = Self::now_min();
        let mut reqs = 0;
        let mut wiki = 0;
        let mut hourly = vec![0u64; 24];

        for i in 0..1440 {
            let m = now.saturating_sub(i);
            let s = &self.slots[(m % 1440) as usize];
            if s.epoch_min.load(Ordering::Relaxed) == m {
                let r = s.requests.load(Ordering::Relaxed);
                let w = s.wiki_calls.load(Ordering::Relaxed);
                reqs += r;
                wiki += w;
                hourly[23 - (i / 60) as usize] += r;
            }
        }
        (
            reqs,
            wiki,
            self.total_facts_harvested.load(Ordering::Relaxed),
            hourly,
        )
    }
}

#[derive(Clone)]
pub struct AppState {
    pub reservoir: Arc<RwLock<Vec<TriviaFact>>>,
    pub dino_cache: Cache<String, Arc<Vec<TriviaFact>>>,
    pub http: reqwest::Client,
    pub wiki_sem: Arc<Semaphore>,
    pub stats: Arc<Stats24h>,
    pub llm_tx: Option<mpsc::Sender<LlmJob>>,
}

#[derive(Deserialize, Default)]
pub struct Params {
    pub format: Option<String>,
    pub q: Option<String>,
}

#[tokio::main]
async fn main() {
    let user_agent = std::env::var("WIKI_USER_AGENT").unwrap_or_else(|_| {
        "dino-trivia/1.0 (https://dino.example.com; contact@example.com) reqwest/0.12".to_string()
    });

    let http = reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(Duration::from_secs(10))
        .build()
        .expect("Failed to initialize HTTP client");

    // Optional LLM upgrade path: only active when OLLAMA_URL is set.
    let llm_url = std::env::var("OLLAMA_URL").ok();
    let (llm_tx, llm_rx) = match &llm_url {
        Some(_) => {
            let (tx, rx) = mpsc::channel(64);
            (Some(tx), Some(rx))
        }
        None => (None, None),
    };

    let state = AppState {
        reservoir: Arc::new(RwLock::new(vec![TriviaFact {
            dino: "Tyrannosaurus".into(),
            category: "Paleobiology".into(),
            fact: "Tyrannosaurus had the highest bite force of any terrestrial animal, capable of crushing solid bone with up to 35,000 to 57,000 newtons of force.".into(),
            wiki_url: "https://en.wikipedia.org/wiki/Tyrannosaurus".into(),
        }])),
        dino_cache: Cache::builder()
            .max_capacity(2_000)
            .time_to_live(Duration::from_secs(86400 * 7))
            .build(),
        http,
        wiki_sem: Arc::new(Semaphore::new(2)),
        stats: Arc::new(Stats24h::new()),
        llm_tx,
    };

    if let (Some(rx), Some(url)) = (llm_rx, llm_url) {
        let model = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "qwen2.5:3b-instruct".into());
        tokio::spawn(llm_worker(rx, state.clone(), LlmCfg { url, model }));
    }

    // Background Harvester Task
    let bg_state = state.clone();
    tokio::spawn(async move {
        for (idx, &dino) in DINO_POOL.iter().cycle().enumerate() {
            let _ = get_or_harvest_dino(dino, &bg_state).await;
            let pause = if idx < 5 {
                Duration::from_secs(2)
            } else {
                Duration::from_secs(45)
            };
            tokio::time::sleep(pause).await;
        }
    });

    // Per-IP Rate Limiter: 3 req/s sustained, 40 burst
    let gov = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(3)
            .burst_size(40)
            .finish()
            .unwrap(),
    );

    let app = Router::new()
        .route("/", get(random_trivia_handler))
        .route("/stats", get(stats_handler))
        .route("/{dino}", get(specific_dino_handler))
        .layer(GovernorLayer { config: gov })
        .with_state(state);

    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let listener = tokio::net::TcpListener::bind(&bind_addr).await.unwrap();
    println!("🦖 Dino Trivia API listening on http://{}", bind_addr);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .unwrap();
}

// --- HTTP Handlers ---

async fn random_trivia_handler(
    headers: HeaderMap,
    Query(params): Query<Params>,
    State(state): State<AppState>,
) -> Response {
    state.stats.rec_req();
    let card = {
        let pool = state.reservoir.read().unwrap();
        pool.choose(&mut rand::thread_rng()).cloned()
    };

    match card {
        Some(fact) => render_fact(&fact, &headers, &params),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Warming up fossil beds...\n",
        )
            .into_response(),
    }
}

async fn specific_dino_handler(
    Path(dino): Path<String>,
    headers: HeaderMap,
    Query(params): Query<Params>,
    State(state): State<AppState>,
) -> Response {
    state.stats.rec_req();
    match get_or_harvest_dino(&dino, &state).await {
        Ok(facts) => {
            if let Some(chosen) = facts.choose(&mut rand::thread_rng()) {
                render_fact(chosen, &headers, &params)
            } else {
                (
                    StatusCode::NOT_FOUND,
                    "No short facts extracted for this dinosaur.\n",
                )
                    .into_response()
            }
        }
        Err(msg) => (StatusCode::NOT_FOUND, format!("🦖 {}\n", msg)).into_response(),
    }
}

// --- Wikipedia Extraction & Trivia Heuristics ---

async fn get_or_harvest_dino(
    raw_name: &str,
    state: &AppState,
) -> Result<Arc<Vec<TriviaFact>>, String> {
    let clean = raw_name.trim().replace(' ', "_").to_lowercase();
    if clean.is_empty()
        || clean.len() > 60
        || !clean.chars().all(|c| c.is_alphanumeric() || c == '_')
    {
        return Err("Invalid dinosaur name.".into());
    }
    let mut chars = clean.chars();
    let canonical = chars.next().unwrap().to_uppercase().collect::<String>() + chars.as_str();

    state
        .dino_cache
        .try_get_with(canonical.clone(), async {
            let _permit = state
                .wiki_sem
                .acquire()
                .await
                .map_err(|_| "Semaphore closed")?;
            tokio::time::sleep(Duration::from_millis(350)).await;
            state.stats.rec_wiki();

            let url = format!(
                "https://en.wikipedia.org/w/api.php?action=query&prop=extracts&explaintext=1&redirects=1&format=json&titles={}",
                canonical
            );

            let resp: serde_json::Value = state
                .http
                .get(&url)
                .send()
                .await
                .map_err(|e| e.to_string())?
                .json()
                .await
                .map_err(|e| e.to_string())?;

            let pages = resp["query"]["pages"]
                .as_object()
                .ok_or("Invalid Wiki response")?;
            let (_, page) = pages.iter().next().ok_or("No page found")?;
            if page.get("missing").is_some() {
                return Err(format!("'{}' not found on Wikipedia.", canonical));
            }

            let title = page["title"].as_str().unwrap_or(&canonical).to_string();
            let extract = page["extract"].as_str().unwrap_or("");

            // Guardrail: verify article is about a dinosaur or prehistoric reptile
            let lower_head = extract.chars().take(1500).collect::<String>().to_lowercase();
            let is_dino = [
                "dinosaur",
                "theropod",
                "sauropod",
                "ceratops",
                "jurassic",
                "cretaceous",
                "triassic",
                "ornithischian",
            ]
            .iter()
            .any(|k| lower_head.contains(k));

            if !is_dino {
                return Err(format!("'{}' does not appear to be a dinosaur.", title));
            }

            let facts = extract_trivia_nuggets(&title, extract);
            if facts.is_empty() {
                return Err("Could not extract trivia sentences.".into());
            }

            // Append newly discovered facts into the global reservoir (bounded at 5,000 facts)
            {
                let mut pool = state.reservoir.write().unwrap();
                if pool.len() < 5_000 {
                    for f in &facts {
                        if !pool.iter().any(|existing| existing.fact == f.fact) {
                            pool.push(f.clone());
                        }
                    }
                    state
                        .stats
                        .total_facts_harvested
                        .store(pool.len() as u64, Ordering::Relaxed);
                }
            }

            // Heuristic facts serve immediately; the LLM worker upgrades them in the background.
            if let Some(tx) = &state.llm_tx {
                let _ = tx.try_send(LlmJob {
                    key: canonical.clone(),
                    title: title.clone(),
                    extract: extract.to_string(),
                    old: facts.clone(),
                });
            }

            Ok(Arc::new(facts))
        })
        .await
        .map_err(|e| e.to_string())
}

const MAX_FACTS_PER_DINO: usize = 8;
const MIN_SCORE: i32 = 6;

const SKIP_SECTIONS: &[&str] = &[
    "see also", "references", "external links", "further reading", "classification",
    "phylogeny", "notes", "bibliography", "gallery",
];
const GOOD_SECTIONS: &[&str] = &[
    "description", "paleobiology", "behavio", "feeding", "diet", "reproduction", "growth",
    "senses", "locomotion", "cultural", "etymology", "discovery", "paleoecology", "pathology",
];
const DRY_SECTIONS: &[&str] = &["history", "taxonomy", "species", "specimen", "systematics"];

// Scholarly hedging and taxonomy boilerplate: never trivia.
const REJECT_PHRASES: &[&str] = &[
    "is a genus of", "type species", "cladogram", "holotype", "paratype", "syntype", "et al",
    "suggested", "proposed", "argued", "hypothes", "disagree", "speculat", "nomen", "synonym",
    "reassign", "according to", "described by", "unknown", "uncertain", "debate", "a paper",
];
// Sentences that only make sense in the context of the previous one.
const DEPENDENT_STARTS: &[&str] = &[
    "however", "additionally", "furthermore", "moreover", "thus", "therefore", "also",
    "although", "while", "despite", "since", "because", "such", "these", "this", "those",
    "that", "they", "he", "she", "both", "another", "others", "then", "later", "but", "and",
    "or", "so", "yet", "instead", "likewise", "similarly", "conversely", "nevertheless",
    "consequently",
];
const TRIVIA_WORDS: &[&str] = &[
    "bite", "teeth", "tooth", "brain", "feather", "horn", "crest", "club", "armor", "armour",
    "spike", "sail", "claw", "vision", "smell", "hearing", "swim", "egg", "nest", "predator",
    "prey", "hunt", "swallow", "stomach", "fast", "speed", "weigh", "tall", "named", "means",
    "derived", "greek", "latin", "mistaken", "injur", "pathology", "herd", "sprint", "tail",
    "skin", "colour", "color", "jaw", "skull", "eat", "venom", "roar", "vocal", "footprint",
    "juvenile", "lifespan", "extinct",
];
const SUPERLATIVES: &[&str] = &[
    "largest", "smallest", "longest", "heaviest", "fastest", "tallest", "oldest", "strongest",
    "highest", "biggest", "first", "only", "most", "unique",
];
const UNITS: &[&str] = &[
    "m", "cm", "mm", "metres", "meters", "ft", "feet", "inches", "kg", "lb", "lbs", "pounds",
    "tonnes", "tons", "kilograms", "km/h", "mph", "kilometres", "kilometers", "million",
    "years",
];

fn is_year(t: &str) -> bool {
    t.len() == 4 && t.parse::<u16>().is_ok_and(|y| (1800..2100).contains(&y))
}

// Splits on ". " + uppercase, skipping initials ("J. Smith") and a few abbreviations.
fn split_sentences(line: &str) -> Vec<&str> {
    let b = line.as_bytes();
    let (mut out, mut start) = (Vec::new(), 0);
    for i in 0..b.len().saturating_sub(2) {
        if matches!(b[i], b'.' | b'!' | b'?') && b[i + 1] == b' ' && b[i + 2].is_ascii_uppercase() {
            let word = line[start..i].rsplit(' ').next().unwrap_or("");
            let initial = word.len() == 1 && word.chars().all(|c| c.is_uppercase());
            if initial || matches!(word, "St" | "Dr" | "Mr" | "Mt" | "sp" | "spp" | "cf" | "vs" | "No") {
                continue;
            }
            out.push(line[start..=i].trim());
            start = i + 2;
        }
    }
    out.push(line[start..].trim());
    out
}

// Specimen catalogue codes like "CM 9401", "NMMNH P-3698", "MOR1125".
fn has_specimen_code(toks: &[&str]) -> bool {
    toks.iter()
        .any(|t| t.chars().any(|c| c.is_ascii_uppercase()) && t.chars().any(|c| c.is_ascii_digit()))
        || toks.windows(2).any(|w| {
            let a = w[0].trim_matches(|c: char| !c.is_alphanumeric());
            a.len() >= 2 && a.chars().all(|c| c.is_ascii_uppercase()) && w[1].starts_with(|c: char| c.is_ascii_digit())
        })
}

fn has_measurement(toks: &[&str]) -> bool {
    toks.windows(2).any(|w| {
        w[0].chars().any(|c| c.is_ascii_digit())
            && UNITS.contains(&w[1].trim_matches(|c: char| !c.is_alphanumeric() && c != '/'))
    })
}

// None = reject outright; Some(score) = candidate, higher is better trivia.
fn score_sentence(s: &str, dino: &str, section: &str) -> Option<i32> {
    let n = s.len();
    if !(60..=200).contains(&n) || !s.starts_with(|c: char| c.is_uppercase()) || !s.ends_with('.') {
        return None;
    }
    let lower = s.to_lowercase();
    if REJECT_PHRASES.iter().any(|p| lower.contains(p)) {
        return None;
    }
    let first = lower.split(|c: char| !c.is_alphabetic()).next().unwrap_or("");
    if DEPENDENT_STARTS.contains(&first) {
        return None;
    }
    let toks: Vec<&str> = s.split_whitespace().collect();
    let cited_year = toks.iter().any(|t| {
        (t.starts_with('(') || t.ends_with(')')) && is_year(t.trim_matches(|c: char| !c.is_ascii_digit()))
    });
    if cited_year || has_specimen_code(&toks) {
        return None;
    }

    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let mut score = 2 * TRIVIA_WORDS.iter().filter(|k| words.iter().any(|w| w.starts_with(**k))).count().min(3) as i32;
    if SUPERLATIVES.iter().any(|k| words.contains(k)) {
        score += 3;
    }
    if has_measurement(&toks) {
        score += 3;
    } else if s.chars().any(|c| c.is_ascii_digit()) {
        score += 1;
    }
    let dino_l = dino.to_lowercase();
    if lower.contains(&dino_l) {
        score += 2;
    }
    if (80..=170).contains(&n) {
        score += 1;
    }
    if toks.iter().any(|t| is_year(t.trim_matches(|c: char| !c.is_ascii_digit()))) {
        score -= 2;
    }
    let names = toks
        .iter()
        .skip(1)
        .filter(|t| {
            t.starts_with(|c: char| c.is_uppercase())
                && !dino_l.contains(&t.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        })
        .count();
    if names >= 3 {
        score -= 3;
    }
    let sec = section.to_lowercase();
    if GOOD_SECTIONS.iter().any(|k| sec.contains(k)) {
        score += 1;
    }
    if DRY_SECTIONS.iter().any(|k| sec.contains(k)) {
        score -= 2;
    }
    (score >= MIN_SCORE).then_some(score)
}

fn extract_trivia_nuggets(dino_title: &str, full_text: &str) -> Vec<TriviaFact> {
    let wiki_url = format!("https://en.wikipedia.org/wiki/{}", dino_title.replace(' ', "_"));
    let (mut top, mut sub) = ("Overview".to_string(), String::new());
    let mut scored: Vec<(i32, TriviaFact)> = Vec::new();

    for raw_line in full_text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("==") && line.ends_with("==") {
            let name = line.trim_matches('=').trim().to_string();
            if line.starts_with("===") {
                sub = name;
            } else {
                top = name;
                sub.clear();
            }
            continue;
        }
        let section = format!("{top} {sub}");
        if SKIP_SECTIONS.iter().any(|s| section.to_lowercase().contains(s)) {
            continue;
        }

        for sentence in split_sentences(line) {
            let mut s = sentence.to_string();
            if s.starts_with("It ") {
                s = format!("{}{}", dino_title, &s[2..]);
            } else if s.starts_with("Its ") {
                s = format!("{}'s {}", dino_title, &s[4..]);
            }
            let Some(score) = score_sentence(&s, dino_title, &section) else { continue };
            if scored.iter().any(|(_, f)| f.fact == s) {
                continue;
            }
            scored.push((
                score,
                TriviaFact {
                    dino: dino_title.to_string(),
                    category: top.clone(),
                    fact: s,
                    wiki_url: wiki_url.clone(),
                },
            ));
        }
    }
    // Stable sort: ties keep article order.
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored.truncate(MAX_FACTS_PER_DINO);
    scored.into_iter().map(|(_, f)| f).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_trivia_drops_scholarly_noise() {
        let text = "Intro line.\n== Description ==\n\
            It could run at speeds of up to 40 kilometres per hour, outpacing most predators of its day. \
            However, the exact age and provenance of CM 9401 remain uncertain due to a lack of detailed field documentation. \
            Lehman and Carpenter (1990) described NMMNH P-3698 as belonging to a new tyrannosaurid genus, while others disagreed. \
            Extrapolating from this, Haas suggested that ankylosaurs ate relatively soft non-abrasive vegetation.\n\
            == References ==\n\
            It had enormous teeth that could crush bone with a force of 57,000 newtons or more.";
        let facts: Vec<String> = extract_trivia_nuggets("Tyrannosaurus", text).into_iter().map(|f| f.fact).collect();
        assert_eq!(facts.len(), 1, "{facts:?}");
        assert!(facts[0].starts_with("Tyrannosaurus could run"), "{facts:?}");
    }

    #[test]
    fn esc_neutralises_markup() {
        assert_eq!(esc("<b>\"x\" & 'y'</b>"), "&lt;b&gt;&quot;x&quot; &amp; &#x27;y&#x27;&lt;/b&gt;");
    }

    #[test]
    fn grounding_rejects_invented_numbers() {
        let art = "Tyrannosaurus had a bite force of up to 57,000 newtons and teeth 30 cm long.";
        assert!(grounded("Tyrannosaurus had teeth 30 cm long and a bite force of 57,000 newtons.", art));
        assert!(!grounded("Tyrannosaurus could run at 70 kilometres per hour across open plains.", art));
    }
}

// --- Optional LLM trivia extraction (Ollama sidecar) ---

pub struct LlmJob {
    key: String,
    title: String,
    extract: String,
    old: Vec<TriviaFact>,
}

struct LlmCfg {
    url: String,
    model: String,
}

const LLM_PROMPT: &str = "You extract dinosaur trivia from an article. Pick 3 to 5 surprising, self-contained facts. \
Rules: each fact is exactly ONE sentence of 60-200 characters; reuse the article's own wording and numbers as closely \
as possible and add nothing that is not in the article; always name the dinosaur explicitly (never 'it' or 'they'); \
prefer sizes, speeds, senses, diet, behaviour, injuries, discovery stories and the meaning of the name; skip \
classification, specimen codes, citations and scientific disputes. Reply with JSON only: {\"facts\": [\"...\"]}";

// One job at a time keeps the CPU-only sidecar from thrashing; the channel bounds the backlog.
async fn llm_worker(mut rx: mpsc::Receiver<LlmJob>, state: AppState, cfg: LlmCfg) {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(900))
        .build()
        .expect("Failed to initialize LLM HTTP client");

    // Wait for the sidecar to finish pulling the model so early jobs aren't burned on retries.
    loop {
        let ready = match http.get(format!("{}/api/tags", cfg.url)).send().await {
            Ok(r) => r.text().await.is_ok_and(|t| t.contains(&cfg.model)),
            Err(_) => false,
        };
        if ready {
            break;
        }
        tokio::time::sleep(Duration::from_secs(15)).await;
    }

    while let Some(job) = rx.recv().await {
        // ponytail: fixed retries cover Ollama still pulling its model at startup; backoff if it matters
        let mut result = Err(String::new());
        for attempt in 0..5 {
            result = llm_facts(&http, &cfg, &job).await;
            if result.is_ok() {
                break;
            }
            if attempt < 4 {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }

        match result {
            Ok(facts) if facts.len() >= 2 => {
                println!("🧠 {}: {} LLM facts replace {} heuristic", job.title, facts.len(), job.old.len());
                state.dino_cache.insert(job.key.clone(), Arc::new(facts.clone())).await;
                let mut pool = state.reservoir.write().unwrap();
                pool.retain(|f| !job.old.iter().any(|o| o.fact == f.fact));
                for f in facts {
                    if pool.len() < 5_000 {
                        pool.push(f);
                    }
                }
                state.stats.total_facts_harvested.store(pool.len() as u64, Ordering::Relaxed);
            }
            Ok(_) => eprintln!("🧠 {}: too few grounded LLM facts, keeping heuristic ones", job.title),
            Err(e) => eprintln!("🧠 {}: LLM failed ({e}), keeping heuristic facts", job.title),
        }
    }
}

async fn llm_facts(http: &reqwest::Client, cfg: &LlmCfg, job: &LlmJob) -> Result<Vec<TriviaFact>, String> {
    let body = serde_json::json!({
        "model": cfg.model,
        "stream": false,
        "format": {
            "type": "object",
            "properties": {"facts": {"type": "array", "items": {"type": "string"}}},
            "required": ["facts"]
        },
        "options": {"temperature": 0.2, "num_ctx": 4096},
        "messages": [
            {"role": "system", "content": LLM_PROMPT},
            {"role": "user", "content": format!("Dinosaur: {}\n\nArticle:\n{}", job.title, article_for_llm(&job.extract))},
        ],
    });
    let resp: serde_json::Value = http
        .post(format!("{}/api/chat", cfg.url))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    let content = resp["message"]["content"].as_str().ok_or("no message content")?;
    let parsed: serde_json::Value = serde_json::from_str(content).map_err(|e| e.to_string())?;
    let raw = parsed["facts"].as_array().ok_or("no facts array")?;

    let name = job.title.split_whitespace().next().unwrap_or("").to_lowercase();
    let wiki_url = format!("https://en.wikipedia.org/wiki/{}", job.title.replace(' ', "_"));
    let mut out: Vec<TriviaFact> = Vec::new();
    for f in raw.iter().filter_map(|v| v.as_str()).map(str::trim) {
        let ok = (50..=220).contains(&f.len())
            && f.starts_with(|c: char| c.is_uppercase())
            && f.ends_with('.')
            && f.to_lowercase().contains(&name)
            && grounded(f, &job.extract)
            && !out.iter().any(|o| o.fact == f);
        if ok {
            out.push(TriviaFact {
                dino: job.title.clone(),
                category: "Trivia".into(),
                fact: f.to_string(),
                wiki_url: wiki_url.clone(),
            });
        }
    }
    out.truncate(MAX_FACTS_PER_DINO);
    Ok(out)
}

// Article text minus dry sections, trimmed to fit the model's 4k context.
fn article_for_llm(extract: &str) -> String {
    let mut skip = false;
    let mut out = String::new();
    for line in extract.lines().map(str::trim) {
        if line.starts_with("==") && line.ends_with("==") && !line.starts_with("===") {
            let l = line.to_lowercase();
            skip = SKIP_SECTIONS.iter().any(|s| l.contains(s));
        }
        if skip || line.is_empty() {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.chars().take(9000).collect()
}

// Guards against small-model invention: every number and ~all long words must appear in the article.
fn grounded(fact: &str, article: &str) -> bool {
    let art = article.to_lowercase();
    let toks: Vec<String> = fact
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 4 || w.chars().any(|c| c.is_ascii_digit()))
        .map(String::from)
        .collect();
    if toks.is_empty() {
        return false;
    }
    let hits = toks.iter().filter(|w| art.contains(w.as_str())).count();
    let numbers_ok = toks
        .iter()
        .filter(|w| w.chars().any(|c| c.is_ascii_digit()))
        .all(|w| art.contains(w.as_str()));
    numbers_ok && hits * 100 >= toks.len() * 85
}

// --- Output Rendering ---

fn render_fact(fact: &TriviaFact, headers: &HeaderMap, params: &Params) -> Response {
    if params.format.as_deref() == Some("json") {
        return Json(fact).into_response();
    }

    if params.q.is_some() || params.format.as_deref() == Some("oneliner") {
        let line = format!("🦖 [{}] {}\n", fact.dino, fact.fact);
        return ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], line).into_response();
    }

    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    let is_cli =
        ua.contains("curl") || ua.contains("wget") || ua.contains("httpie") || ua.contains("fetch");

    if is_cli || params.format.as_deref() == Some("text") {
        let wrapped = wrap_lines(&fact.fact, 52);
        let mut out = String::new();
        out.push_str(&format!(
            "\n  \x1b[1;33m🦕 DINO TRIVIA: {}\x1b[0m \x1b[2m({})\x1b[0m\n",
            fact.dino.to_uppercase(),
            fact.category
        ));
        out.push_str("  \x1b[36m╭──────────────────────────────────────────────────────╮\x1b[0m\n");
        for l in wrapped {
            out.push_str(&format!("  \x1b[36m│\x1b[0m {:<52} \x1b[36m│\x1b[0m\n", l));
        }
        out.push_str("  \x1b[36m╰───────────────────┬──────────────────────────────────╯\x1b[0m\n");
        out.push_str("       \x1b[1;32m__\x1b[0m             \x1b[36m│\x1b[0m\n");
        out.push_str("      \x1b[1;32m/ _)  \x1b[36m──────────╯\x1b[0m   \x1b[2mSource: Wikipedia (CC BY-SA)\x1b[0m\n");
        out.push_str(&format!(
            "  \x1b[1;32m_.~ /_\x1b[0m                  \x1b[2m{}\x1b[0m\n\n",
            fact.wiki_url
        ));

        return ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], out).into_response();
    }

    let html = format!(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><title>Dino Trivia — {}</title>
        <style>body{{background:#0d1117;color:#e6edf3;font-family:ui-monospace,monospace;max-width:680px;margin:4rem auto;padding:0 1.2rem}}
        .box{{background:#161b22;border:1px solid #30363d;border-radius:10px;padding:1.8rem;margin:1.5rem 0;box-shadow:0 8px 24px rgba(0,0,0,0.4)}}
        .tag{{background:#238636;color:#fff;padding:0.2rem 0.6rem;border-radius:4px;font-size:0.8rem}}
        .fact{{font-size:1.2rem;line-height:1.6;margin:1.2rem 0}}
        code{{background:#21262d;padding:0.2rem 0.5rem;border-radius:4px;color:#79c0ff}}
        a{{color:#58a6ff;text-decoration:none}}</style></head><body>
        <h1>🦖 Random Dinosaur Trivia</h1>
        <div class="box">
            <div><strong>{}</strong> &nbsp;<span class="tag">{}</span></div>
            <p class="fact">"{}"</p>
            <small>Source: <a href="{}">Wikipedia ({})</a> • CC BY-SA 4.0</small>
        </div>
        <p><a href="/">🔄 Another Random Fact</a> &nbsp;|&nbsp; <a href="/stats">📊 24h Live Stats</a></p>
        <hr style="border:0;border-top:1px solid #30363d;margin:2rem 0">
        <p><small>Terminal usage:<br>
        • Random card: <code>curl -s https://dino.yourdomain.com</code><br>
        • Specific dino: <code>curl -s https://dino.yourdomain.com/ankylosaurus</code><br>
        • One-liner (for .bashrc): <code>curl -s "https://dino.yourdomain.com?q"</code></small></p>
        </body></html>"#,
        esc(&fact.dino), esc(&fact.dino), esc(&fact.category), esc(&fact.fact), esc(&fact.wiki_url), esc(&fact.dino)
    );
    (
        [(header::CONTENT_SECURITY_POLICY, "default-src 'none'; style-src 'unsafe-inline'")],
        Html(html),
    )
        .into_response()
}

// Facts come from Wikipedia/LLM text, so escape everything interpolated into HTML.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

fn wrap_lines(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.len() + word.len() + 1 > width && !current.is_empty() {
            lines.push(current);
            current = String::new();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

async fn stats_handler(State(state): State<AppState>) -> Html<String> {
    let (reqs, wiki_calls, pool_size, hourly) = state.stats.snapshot();
    let max_h = *hourly.iter().max().unwrap_or(&1).max(&1);
    let bars: String = hourly
        .iter()
        .enumerate()
        .map(|(i, &r)| {
            let h = ((r as f64 / max_h as f64) * 85.0) as u32;
            let x = i * 24 + 8;
            let y = 95 - h;
            format!(
                r##"<rect x="{x}" y="{y}" width="18" height="{h}" fill="#3fb950" rx="2"><title>{r} reqs</title></rect>"##
            )
        })
        .collect();

    Html(format!(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>24h Stats — Dino Trivia</title>
        <style>body{{background:#0d1117;color:#e6edf3;font-family:monospace;max-width:680px;margin:4rem auto;padding:0 1.2rem}}
        .grid{{display:grid;grid-template-columns:repeat(3,1fr);gap:1rem;margin:1.5rem 0}}
        .card{{background:#161b22;border:1px solid #30363d;padding:1rem;border-radius:8px}}
        .val{{font-size:1.7rem;color:#3fb950;font-weight:bold;margin-top:0.3rem}}</style></head><body>
        <h1>📊 Last 24h Telemetry</h1>
        <div class="grid">
            <div class="card"><div>24h Trivia Served</div><div class="val">{reqs}</div></div>
            <div class="card"><div>Facts in Reservoir</div><div class="val">{pool_size}</div></div>
            <div class="card"><div>24h Wiki API Calls</div><div class="val">{wiki_calls}</div></div>
        </div>
        <h3>Hourly Requests (Last 24h)</h3>
        <svg viewBox="0 0 585 100" style="background:#161b22;border:1px solid #30363d;border-radius:8px;width:100%">{bars}</svg>
        <p><a href="/" style="color:#58a6ff">← Back to random trivia</a></p>
        </body></html>"#
    ))
}
