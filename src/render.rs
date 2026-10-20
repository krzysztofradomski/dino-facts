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

pub const SITE: &str = "https://dinofacts.fyi";
const SITE_TITLE: &str = "Dino Trivia — Random Dinosaur Facts for Browser & Terminal";
const SITE_DESC: &str = "A random, sourced dinosaur fact on every visit, from 1,400+ Wikipedia genera: diet, \
habitat, era and anatomy. Free JSON API and curl-friendly terminal cards.";

// <head> shared by every HTML page: SEO basics plus Open Graph / Twitter cards for link previews.
fn head(title: &str, desc: &str, path: &str) -> String {
    let (title, desc, url) = (esc(title), esc(desc), format!("{SITE}{path}"));
    format!(
        r##"<meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
        <title>{title}</title><meta name="description" content="{desc}">
        <link rel="canonical" href="{url}"><link rel="icon" href="/favicon.svg" type="image/svg+xml">
        <meta name="theme-color" content="#0d1117">
        <meta property="og:type" content="website"><meta property="og:site_name" content="Dino Trivia">
        <meta property="og:title" content="{title}"><meta property="og:description" content="{desc}">
        <meta property="og:url" content="{url}"><meta property="og:image" content="{SITE}/og.png">
        <meta property="og:image:width" content="1200"><meta property="og:image:height" content="630">
        <meta property="og:image:alt" content="Dino Trivia: a random, sourced dinosaur fact. curl dinofacts.fyi">
        <meta name="twitter:card" content="summary_large_image"><meta name="twitter:title" content="{title}">
        <meta name="twitter:description" content="{desc}"><meta name="twitter:image" content="{SITE}/og.png">"##
    )
}

// Search snippets cut off around 160 characters; trim on a word boundary.
fn snippet(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max - 1).collect();
    format!("{}…", cut.rsplit_once(' ').map_or(cut.as_str(), |(a, _)| a))
}

// `home` = the random-fact homepage, which keeps a stable title and description for search and sharing.
pub fn render_fact(fact: &TriviaFact, headers: &HeaderMap, params: &Params, home: bool) -> Response {
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

    let head = if home {
        // JSON-LD is data, not script, so the strict CSP doesn't block it.
        let ld = format!(
            r#"<script type="application/ld+json">{{"@context":"https://schema.org","@type":"WebSite","name":"Dino Trivia","url":"{SITE}","description":"{}"}}</script>"#,
            SITE_DESC.replace('"', "\\\"")
        );
        head(SITE_TITLE, SITE_DESC, "/") + &ld
    } else {
        let desc = snippet(&format!("{} (Source: Wikipedia)", fact.fact), 160);
        head(&format!("{} Facts — Dino Trivia", fact.dino), &desc, &format!("/{}", fact.dino.replace(' ', "_")))
    };
    let html = format!(
        r#"<!DOCTYPE html><html lang="en"><head>{head}
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
        esc(&fact.dino), esc(&fact.category), esc(&fact.fact), esc(&fact.wiki_url), esc(&fact.dino)
    );
    (
        [(header::CONTENT_SECURITY_POLICY, "default-src 'none'; style-src 'unsafe-inline'; img-src 'self'")],
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
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><title>24h Stats — Dino Trivia</title>
        <meta name="viewport" content="width=device-width, initial-scale=1"><meta name="robots" content="noindex">
        <link rel="icon" href="/favicon.svg" type="image/svg+xml">
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

    use axum::{body::to_bytes, http::HeaderValue};

    fn fact(text: &str) -> TriviaFact {
        TriviaFact {
            dino: "Tyrannosaurus".into(),
            category: "Description".into(),
            fact: text.into(),
            wiki_url: "https://en.wikipedia.org/wiki/Tyrannosaurus".into(),
        }
    }

    fn params(format: Option<&str>, q: bool) -> Params {
        Params { format: format.map(String::from), q: q.then(String::new) }
    }

    async fn render(ua: &str, p: Params, f: &TriviaFact) -> (axum::http::response::Parts, String) {
        render_page(ua, p, f, false).await
    }

    async fn render_page(ua: &str, p: Params, f: &TriviaFact, home: bool) -> (axum::http::response::Parts, String) {
        let mut headers = HeaderMap::new();
        headers.insert(header::USER_AGENT, HeaderValue::from_str(ua).unwrap());
        let (parts, body) = render_fact(f, &headers, &p, home).into_parts();
        let bytes = to_bytes(body, usize::MAX).await.unwrap();
        (parts, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn json_format_returns_the_fact_as_json() {
        let (parts, body) = render("curl/8", params(Some("json"), false), &fact("Big teeth.")).await;
        assert_eq!(parts.headers[header::CONTENT_TYPE], "application/json");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!((v["dino"].as_str(), v["fact"].as_str()), (Some("Tyrannosaurus"), Some("Big teeth.")));
    }

    #[tokio::test]
    async fn q_returns_a_single_plain_line() {
        let (parts, body) = render("Mozilla/5.0", params(None, true), &fact("Big teeth.")).await;
        assert_eq!(parts.headers[header::CONTENT_TYPE], "text/plain; charset=utf-8");
        assert_eq!(body, "🦖 [Tyrannosaurus] Big teeth.\n");
    }

    #[tokio::test]
    async fn cli_user_agents_get_the_ansi_card() {
        for ua in ["curl/8.4", "Wget/1.21", "HTTPie/3.2", "node-fetch"] {
            let (_, body) = render(ua, params(None, false), &fact("Big teeth.")).await;
            assert!(body.contains("\x1b[") && body.contains("DINO TRIVIA: TYRANNOSAURUS"), "{ua}");
        }
    }

    #[tokio::test]
    async fn format_text_forces_the_ansi_card_for_browsers() {
        let (_, body) = render("Mozilla/5.0", params(Some("text"), false), &fact("Big teeth.")).await;
        assert!(body.contains("\x1b["));
    }

    #[tokio::test]
    async fn browsers_get_html_with_csp_and_escaped_content() {
        let evil = fact("<script>alert(1)</script> & more");
        let (parts, body) = render("Mozilla/5.0", params(None, false), &evil).await;
        assert!(parts.headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
        assert!(parts.headers.contains_key(header::CONTENT_SECURITY_POLICY));
        assert!(!body.contains("<script>"));
        assert!(body.contains("&lt;script&gt;alert(1)&lt;/script&gt; &amp; more"));
    }

    #[tokio::test]
    async fn html_has_share_and_seo_tags() {
        let long = fact(&format!("Tyrannosaurus \"rex\" {}", "had big teeth and ".repeat(20)));
        let (_, page) = render_page("Mozilla/5.0", params(None, false), &long, false).await;
        assert!(page.contains("<title>Tyrannosaurus Facts — Dino Trivia</title>"));
        assert!(page.contains(r#"<link rel="canonical" href="https://dinofacts.fyi/Tyrannosaurus">"#));
        assert!(page.contains(r#"property="og:image" content="https://dinofacts.fyi/og.png""#));
        assert!(page.contains("name=\"viewport\""));
        let desc = page.split(r#"name="description" content=""#).nth(1).unwrap().split('"').next().unwrap();
        assert!(desc.ends_with('…') && desc.chars().count() <= 200, "{desc}");
        assert!(desc.contains("&quot;rex&quot;"));

        let (_, home) = render_page("Mozilla/5.0", params(None, false), &long, true).await;
        assert!(home.contains(&format!("<title>{}</title>", esc(SITE_TITLE))));
        assert!(home.contains(r#"href="https://dinofacts.fyi/">"#) && home.contains("application/ld+json"));
    }

    #[test]
    fn wrap_lines_respects_width() {
        let text = "one two three four five six seven eight nine ten";
        let lines = wrap_lines(text, 12);
        assert!(lines.iter().all(|l| l.len() <= 12), "{lines:?}");
        assert_eq!(lines.join(" "), text);
    }

    #[test]
    fn stats_page_shows_counters_and_24_bars() {
        let html = stats_page(7, 3, 99, &[0u64; 24]);
        assert!(html.contains(r#"24h Trivia Served</div><div class="val">7</div>"#));
        assert!(html.contains(r#"Facts in Reservoir</div><div class="val">99</div>"#));
        assert!(html.contains(r#"24h Wiki API Calls</div><div class="val">3</div>"#));
        assert_eq!(html.matches("<rect").count(), 24);
    }
}
