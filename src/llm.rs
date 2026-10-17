use crate::{
    extract::{TriviaFact, MAX_FACTS_PER_DINO, SKIP_SECTIONS},
    AppState,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;

// --- Optional LLM trivia extraction (Ollama sidecar) ---

pub struct LlmJob {
    pub key: String,
    pub title: String,
    pub extract: String,
    pub old: Vec<TriviaFact>,
}

pub struct LlmCfg {
    pub url: String,
    pub model: String,
}

const LLM_PROMPT: &str = "You extract dinosaur trivia from an article. Pick 3 to 5 surprising, self-contained facts. \
Rules: each fact is exactly ONE sentence of 60-200 characters; reuse the article's own wording and numbers as closely \
as possible and add nothing that is not in the article; always name the dinosaur explicitly (never 'it' or 'they'); \
prefer sizes, speeds, senses, diet, behaviour, injuries, discovery stories and the meaning of the name; skip \
classification, specimen codes, citations and scientific disputes. Reply with JSON only: {\"facts\": [\"...\"]}";

// One job at a time keeps the CPU-only sidecar from thrashing; the channel bounds the backlog.
pub async fn llm_worker(mut rx: mpsc::Receiver<LlmJob>, state: AppState, cfg: LlmCfg) {
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
                state.stats.set_facts(pool.len() as u64);
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
        // num_thread matches the sidecar's 3-CPU quota (extra threads get throttled to a crawl);
        // num_predict stops a runaway generation from holding the single worker.
        "options": {"temperature": 0.2, "num_ctx": 4096, "num_thread": 3, "num_predict": 500},
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grounding_rejects_invented_numbers() {
        let art = "Tyrannosaurus had a bite force of up to 57,000 newtons and teeth 30 cm long.";
        assert!(grounded("Tyrannosaurus had teeth 30 cm long and a bite force of 57,000 newtons.", art));
        assert!(!grounded("Tyrannosaurus could run at 70 kilometres per hour across open plains.", art));
    }

    #[test]
    fn article_for_llm_drops_dry_sections_and_caps_length() {
        let text = format!(
            "Intro text.\n== Description ==\nGood body.\n== References ==\nCite me.\n== Behavior ==\n{}",
            "x".repeat(20_000)
        );
        let out = article_for_llm(&text);
        assert!(out.contains("Good body.") && !out.contains("Cite me."));
        assert_eq!(out.chars().count(), 9000);
    }
}
