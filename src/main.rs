mod extract;
mod genera;
mod llm;
mod render;
mod reservoir;
mod stats;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use extract::{extract_trivia_nuggets, TriviaFact};
use llm::{llm_worker, LlmCfg, LlmJob};
use moka::future::Cache;
use rand::seq::SliceRandom;
use render::{render_fact, Params};
use reservoir::{Reservoir, RESERVOIR_CAP};
use stats::Stats24h;
use std::{sync::Arc, time::Duration};
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

#[derive(Clone)]
pub struct AppState {
    pub reservoir: Arc<Reservoir>,
    pub dino_cache: Cache<String, Arc<Vec<TriviaFact>>>,
    pub http: reqwest::Client,
    pub wiki_sem: Arc<Semaphore>,
    pub stats: Arc<Stats24h>,
    pub llm_tx: Option<mpsc::Sender<LlmJob>>,
}

#[tokio::main]
async fn main() {
    let user_agent = std::env::var("WIKI_USER_AGENT").unwrap_or_else(|_| {
        "dino-trivia/1.0 (https://dinofacts.fyi; hello@radomski.dev) reqwest/0.12".to_string()
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
        reservoir: Arc::new(Reservoir::new(
            vec![TriviaFact {
                dino: "Tyrannosaurus".into(),
                category: "Paleobiology".into(),
                fact: "Tyrannosaurus had the highest bite force of any terrestrial animal, capable of crushing solid bone with up to 35,000 to 57,000 newtons of force.".into(),
                wiki_url: "https://en.wikipedia.org/wiki/Tyrannosaurus".into(),
            }],
            RESERVOIR_CAP,
        )),
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
        let threads = std::env::var("OLLAMA_NUM_THREAD").ok().and_then(|v| v.parse().ok()).unwrap_or(2);
        tokio::spawn(llm_worker(rx, state.clone(), LlmCfg { url, model, threads }));
    }

    // Background harvester: the built-in seeds first, then every genus Wikipedia lists, in random order.
    // Only real fetches are paced; already-cached names are skipped instantly.
    let bg_state = state.clone();
    tokio::spawn(async move {
        let mut fetched = 0usize;
        loop {
            let mut discovered = match genera::fetch(&bg_state).await {
                Ok(list) => {
                    println!("🦕 discovered {} dinosaur genera on Wikipedia", list.len());
                    list
                }
                Err(e) => {
                    eprintln!("🦕 genus discovery failed ({e}); falling back to the built-in seed list");
                    Vec::new()
                }
            };
            discovered.retain(|n| canonical_name(n).is_ok() && !DINO_POOL.contains(&n.as_str()));
            discovered.shuffle(&mut rand::thread_rng());
            let order: Vec<String> = DINO_POOL.iter().map(|s| s.to_string()).chain(discovered).collect();

            for name in order {
                let Ok(key) = canonical_name(&name) else { continue };
                let cached = bg_state.dino_cache.contains_key(&key);
                let _ = get_or_harvest_dino(&name, &bg_state).await;
                if !cached {
                    fetched += 1;
                    tokio::time::sleep(Duration::from_secs(if fetched <= 5 { 2 } else { 45 })).await;
                }
            }
            // Pass complete: wait before re-reading the list so a fully cached pass can't spin.
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    });

    // Per-IP Rate Limiter: 3 req/s sustained, 40 burst
    let gov = Arc::new(
        GovernorConfigBuilder::default()
            // per_second(n) is the refill *interval* in seconds, so per_second(3) meant 1 request / 3 s.
            .per_millisecond(333)
            .burst_size(40)
            .finish()
            .unwrap(),
    );

    let app = router(state).layer(GovernorLayer { config: gov });

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

fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(random_trivia_handler))
        .route("/stats", get(stats_handler))
        .route("/{dino}", get(specific_dino_handler))
        .with_state(state)
}

// --- HTTP Handlers ---

async fn random_trivia_handler(
    headers: HeaderMap,
    Query(params): Query<Params>,
    State(state): State<AppState>,
) -> Response {
    state.stats.rec_req();
    match state.reservoir.random() {
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

// Validates a visitor- or harvester-supplied name and returns Wikipedia's casing ("tyrannosaurus" -> "Tyrannosaurus").
fn canonical_name(raw: &str) -> Result<String, String> {
    let clean = raw.trim().replace(' ', "_").to_lowercase();
    if clean.is_empty() || clean.len() > 60 || !clean.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Err("Invalid dinosaur name.".into());
    }
    let mut chars = clean.chars();
    Ok(chars.next().unwrap().to_uppercase().collect::<String>() + chars.as_str())
}

async fn get_or_harvest_dino(
    raw_name: &str,
    state: &AppState,
) -> Result<Arc<Vec<TriviaFact>>, String> {
    let canonical = canonical_name(raw_name)?;

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

            // Replace this dinosaur's facts in the global pool (random others are evicted when full).
            println!("🦕 {}: {} facts harvested", title, facts.len());
            let pool_len = state.reservoir.replace_dino(&title, &facts);
            state.stats.set_facts(pool_len as u64);

            // Heuristic facts serve immediately; the LLM worker upgrades them in the background.
            if let Some(tx) = &state.llm_tx {
                let _ = tx.try_send(LlmJob {
                    key: canonical.clone(),
                    title: title.clone(),
                    extract: extract.to_string(),
                });
            }

            Ok(Arc::new(facts))
        })
        .await
        .map_err(|e| e.to_string())
}

async fn stats_handler(State(state): State<AppState>) -> Html<String> {
    let (reqs, wiki_calls, pool_size, hourly) = state.stats.snapshot();
    Html(render::stats_page(reqs, wiki_calls, pool_size, &hourly))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use tower::ServiceExt;

    fn state(facts: Vec<TriviaFact>) -> AppState {
        AppState {
            reservoir: Arc::new(Reservoir::new(facts, 100)),
            dino_cache: Cache::builder().max_capacity(10).build(),
            http: reqwest::Client::new(),
            wiki_sem: Arc::new(Semaphore::new(2)),
            stats: Arc::new(Stats24h::new()),
            llm_tx: None,
        }
    }

    fn fact(dino: &str, text: &str) -> TriviaFact {
        TriviaFact {
            dino: dino.into(),
            category: "Description".into(),
            fact: text.into(),
            wiki_url: format!("https://en.wikipedia.org/wiki/{dino}"),
        }
    }

    async fn get(state: &AppState, uri: &str) -> (StatusCode, String) {
        let resp = router(state.clone())
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn root_serves_from_the_reservoir() {
        let s = state(vec![fact("Tyrannosaurus", "Big teeth.")]);
        let (status, body) = get(&s, "/?q").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "🦖 [Tyrannosaurus] Big teeth.\n");
    }

    #[tokio::test]
    async fn empty_reservoir_is_503() {
        let (status, _) = get(&state(vec![]), "/").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn cached_dino_is_served_without_hitting_wikipedia() {
        let s = state(vec![]);
        let cached = Arc::new(vec![fact("Ankylosaurus", "Armoured tail club.")]);
        s.dino_cache.insert("Ankylosaurus".into(), cached).await;
        let (status, body) = get(&s, "/ankylosaurus?q").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "🦖 [Ankylosaurus] Armoured tail club.\n");
        assert_eq!(s.stats.snapshot().1, 0);
    }

    #[tokio::test]
    async fn invalid_names_are_rejected_before_any_network_call() {
        let s = state(vec![]);
        let (status, body) = get(&s, "/bad$name").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("Invalid dinosaur name."), "{body}");
        assert_eq!(s.stats.snapshot().1, 0);
    }

    #[tokio::test]
    async fn stats_page_counts_served_requests() {
        let s = state(vec![fact("Tyrannosaurus", "Big teeth.")]);
        get(&s, "/").await;
        get(&s, "/?format=json").await;
        let (status, body) = get(&s, "/stats").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(r#"24h Trivia Served</div><div class="val">2</div>"#), "{body}");
    }
}
