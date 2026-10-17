use crate::extract::TriviaFact;
use axum::{
    http::{header, HeaderMap},
    response::{Html, IntoResponse, Response},
    Json,
};
use serde::Deserialize;

#[derive(Deserialize, Default)]
pub struct Params {
    pub format: Option<String>,
    pub q: Option<String>,
}

// --- Output Rendering ---

pub fn render_fact(fact: &TriviaFact, headers: &HeaderMap, params: &Params) -> Response {
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
        • Random card: <code>curl -s https://dinofacts.fyi</code><br>
        • Specific dino: <code>curl -s https://dinofacts.fyi/ankylosaurus</code><br>
        • One-liner (for .bashrc): <code>curl -s "https://dinofacts.fyi?q"</code></small></p>
        <p><small><a href="https://github.com/krzysztofradomski/dino-facts">github.com/krzysztofradomski/dino-facts</a></small></p>
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

pub fn stats_page(reqs: u64, wiki_calls: u64, pool_size: u64, hourly: &[u64]) -> String {
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

    format!(
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
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_neutralises_markup() {
        assert_eq!(esc("<b>\"x\" & 'y'</b>"), "&lt;b&gt;&quot;x&quot; &amp; &#x27;y&#x27;&lt;/b&gt;");
    }
}
