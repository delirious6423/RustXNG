//! Search suggestions, ported from `searx/autocomplete.py`.

use std::collections::BTreeSet;

use crate::cache::Cache;
use crate::engine::util::{json_str, locale_language};
use crate::engine::{EngineRequest, EngineResponse};
use crate::network::Network;
use crate::settings::Settings;

/// Autocomplete backends, in the order they are tried.
pub static BACKENDS: &[&str] = &["duckduckgo", "google", "wikipedia", "mwmbl"];

/// Fetch suggestions for `query` from `backend`.
///
/// Every keystroke asks for a prefix; with the response cache, a prefix seen
/// before (typing it again, backspacing) is answered without a round trip.
pub async fn suggest(
    network: &Network,
    cache: Option<&Cache>,
    settings: &Settings,
    backend: &str,
    query: &str,
    locale: &str,
) -> Vec<String> {
    let (url, is_json) = match backend {
        "google" => (
            format!(
                "https://suggestqueries.google.com/complete/search?client=firefox&q={}",
                crate::engine::util::encode(query)
            ),
            true,
        ),
        "wikipedia" => {
            let language = locale_language(locale);
            let language = if language == "all" {
                "en".to_string()
            } else {
                language
            };
            (
                format!(
                    "https://{language}.wikipedia.org/w/api.php?action=opensearch&limit=10&format=json&search={}",
                    crate::engine::util::encode(query)
                ),
                true,
            )
        }
        "mwmbl" => (
            format!(
                "https://api.mwmbl.org/api/v1/search/?s={}",
                crate::engine::util::encode(query)
            ),
            true,
        ),
        // duckduckgo is the default backend
        _ => (
            format!(
                "https://duckduckgo.com/ac/?type=list&q={}",
                crate::engine::util::encode(query)
            ),
            true,
        ),
    };
    let _ = is_json;
    let request = EngineRequest::get(url).header("Accept", "application/json");
    let timeout = std::time::Duration::from_secs_f64(settings.search.timeout_limit.clamp(1.0, 3.0));
    match network.fetch(request, timeout, cache, None).await {
        Ok(response) => parse_suggestions(backend, &response),
        Err(error) => {
            tracing::debug!(backend, %error, "autocomplete backend failed");
            Vec::new()
        }
    }
}

/// Extract the suggestions from a backend response.
pub fn parse_suggestions(backend: &str, response: &EngineResponse) -> Vec<String> {
    let Ok(json) = response.json() else {
        return Vec::new();
    };
    let mut suggestions = BTreeSet::new();
    match backend {
        "wikipedia" => {
            if let Some(items) = json_path_array(&json, 1) {
                for item in items {
                    if let Some(text) = item.as_str() {
                        suggestions.insert(text.to_string());
                    }
                }
            }
        }
        "mwmbl" => {
            if let Some(items) = json.as_array() {
                for item in items.iter().take(10) {
                    let title = json_str(item, "title.0.value");
                    if !title.is_empty() {
                        suggestions.insert(title);
                    }
                }
            }
        }
        // duckduckgo: [{"phrase": "..."}], google: ["query", ["...", ...]]
        _ => {
            if let Some(items) = json.as_array() {
                for item in items {
                    if let Some(text) = item.as_str() {
                        suggestions.insert(text.to_string());
                    } else {
                        let phrase = json_str(item, "phrase");
                        if !phrase.is_empty() {
                            suggestions.insert(phrase);
                        }
                    }
                }
            }
            if let Some(items) = json_path_array(&json, 1) {
                for item in items {
                    if let Some(text) = item.as_str() {
                        suggestions.insert(text.to_string());
                    }
                }
            }
        }
    }
    suggestions.into_iter().take(10).collect()
}

fn json_path_array(value: &serde_json::Value, index: usize) -> Option<&[serde_json::Value]> {
    value.as_array()?.get(index)?.as_array().map(Vec::as_slice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::EngineResponse;

    fn response(body: &str) -> EngineResponse {
        EngineResponse::new(200, "https://example.com/", body.as_bytes().to_vec())
    }

    /// A cached backend answer is served without touching the network (the
    /// proxy below is unreachable, so a network request would return nothing).
    #[tokio::test]
    async fn cached_suggestions_skip_the_network() {
        let mut settings = Settings::defaults();
        settings
            .outgoing
            .proxies
            .insert("all".into(), "http://127.0.0.1:9".into());
        let network = Network::from_settings(&settings).unwrap();
        let cache = Cache::default();
        let url = "https://duckduckgo.com/ac/?type=list&q=rust";
        cache
            .set(
                &Cache::key("GET", url, ""),
                crate::cache::CachedResponse {
                    status: 200,
                    final_url: url.into(),
                    headers: Default::default(),
                    body: std::sync::Arc::new(br#"["rust",["rust","rust book"]]"#.to_vec()),
                },
            )
            .await;
        let suggestions = suggest(
            &network,
            Some(&cache),
            &settings,
            "duckduckgo",
            "rust",
            "en",
        )
        .await;
        assert_eq!(
            suggestions,
            vec!["rust".to_string(), "rust book".to_string()]
        );
    }

    #[test]
    fn duckduckgo_suggestions_are_parsed() {
        let body = r#"[{"phrase":"rust"},{"phrase":"rust book"}]"#;
        assert_eq!(
            parse_suggestions("duckduckgo", &response(body)),
            vec!["rust".to_string(), "rust book".to_string()]
        );
    }

    #[test]
    fn google_suggestions_are_parsed() {
        let body = r#"["rust",["rust","rust book","rustup"]]"#;
        assert_eq!(
            parse_suggestions("google", &response(body)),
            vec![
                "rust".to_string(),
                "rust book".to_string(),
                "rustup".to_string()
            ]
        );
    }

    #[test]
    fn wikipedia_suggestions_are_parsed() {
        let body = r#"["rust",["Rust","Rust (programming language)"]]"#;
        assert_eq!(
            parse_suggestions("wikipedia", &response(body)),
            vec![
                "Rust".to_string(),
                "Rust (programming language)".to_string()
            ]
        );
    }

    #[test]
    fn broken_json_yields_no_suggestions() {
        assert!(parse_suggestions("google", &response("<html>")).is_empty());
    }
}
