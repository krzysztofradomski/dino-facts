use crate::AppState;
use std::time::Duration;

const CATEGORY: &str = "Category:Dinosaur genera";
// 500 titles per page; the category holds ~1,400 articles.
const MAX_PAGES: usize = 10;

// Lists genus article titles from Wikipedia's category, sharing the app's concurrency cap and pacing.
pub async fn fetch(state: &AppState) -> Result<Vec<String>, String> {
    let mut titles = Vec::new();
    let mut cont: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let _permit = state.wiki_sem.acquire().await.map_err(|_| "Semaphore closed")?;
        tokio::time::sleep(Duration::from_millis(350)).await;
        state.stats.rec_wiki();

        let mut req = state.http.get("https://en.wikipedia.org/w/api.php").query(&[
            ("action", "query"),
            ("list", "categorymembers"),
            ("cmtitle", CATEGORY),
            ("cmtype", "page"),
            ("cmnamespace", "0"),
            ("cmlimit", "500"),
            ("format", "json"),
        ]);
        if let Some(c) = &cont {
            req = req.query(&[("cmcontinue", c.as_str())]);
        }
        let resp: serde_json::Value = req
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;

        let (page, next) = parse_members(&resp);
        titles.extend(page);
        match next {
            Some(n) => cont = Some(n),
            None => break,
        }
    }
    Ok(titles)
}

fn parse_members(v: &serde_json::Value) -> (Vec<String>, Option<String>) {
    let titles = v["query"]["categorymembers"]
        .as_array()
        .map(|a| a.iter().filter_map(|m| m["title"].as_str().map(String::from)).collect())
        .unwrap_or_default();
    (titles, v["continue"]["cmcontinue"].as_str().map(String::from))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_titles_and_continuation_token() {
        let page = serde_json::json!({
            "continue": {"cmcontinue": "page|abc|1", "continue": "-||"},
            "query": {"categorymembers": [
                {"pageid": 1, "ns": 0, "title": "Aardonyx"},
                {"pageid": 2, "ns": 0, "title": "Abelisaurus"}
            ]}
        });
        assert_eq!(
            parse_members(&page),
            (vec!["Aardonyx".to_string(), "Abelisaurus".to_string()], Some("page|abc|1".to_string()))
        );
    }

    #[test]
    fn last_page_has_no_continuation() {
        let last = serde_json::json!({"query": {"categorymembers": []}});
        assert_eq!(parse_members(&last), (vec![], None));
    }
}
