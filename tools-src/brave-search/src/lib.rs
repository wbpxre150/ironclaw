//! Brave Search WASM Tool for IronClaw.
//!
//! Provides web and news search via the Brave Search API.
//! Supports filtering by count, country, and freshness.
//!
//! # Authentication
//!
//! Store your Brave Search API key:
//! `ironclaw secret set brave_search_api_key <key>`
//!
//! Get a free API key (2,000 queries/month) at:
//! https://brave.com/search/api/

wit_bindgen::generate!({
    world: "sandboxed-tool",
    path: "../../wit/tool.wit",
});

use serde::Deserialize;

fn default_action() -> String {
    "web_search".to_string()
}

/// Flat parameter struct for brave search.
///
/// Uses a default for `action` so callers that just pass `{"query":"..."}` get
/// a web search rather than a confusing "missing field 'action'" error.
#[derive(Debug, Deserialize)]
struct BraveSearchParams {
    #[serde(default = "default_action")]
    action: String,
    query: String,
    count: Option<u8>,
    country: Option<String>,
    freshness: Option<String>,
}

/// Percent-encode a string for use as a URL query parameter value.
fn url_encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(char::from(b"0123456789ABCDEF"[(b >> 4) as usize]));
                out.push(char::from(b"0123456789ABCDEF"[(b & 0xf) as usize]));
            }
        }
    }
    out
}

struct BraveSearchTool;

impl exports::near::agent::tool::Guest for BraveSearchTool {
    fn execute(req: exports::near::agent::tool::Request) -> exports::near::agent::tool::Response {
        match execute_inner(&req.params) {
            Ok(result) => exports::near::agent::tool::Response {
                output: Some(result),
                error: None,
            },
            Err(e) => exports::near::agent::tool::Response {
                output: None,
                error: Some(e),
            },
        }
    }

    fn schema() -> String {
        SCHEMA.to_string()
    }

    fn description() -> String {
        "Brave Search integration for web and news search. \
         Supports filtering by result count, country, and freshness (past day/week/month/year). \
         Authentication is handled via the 'brave_search_api_key' secret injected by the host."
            .to_string()
    }
}

fn execute_inner(params: &str) -> Result<String, String> {
    let p: BraveSearchParams =
        serde_json::from_str(params).map_err(|e| format!("Invalid parameters: {e}"))?;

    // Pre-flight check: ensure API key exists in secret store.
    // The actual key is injected by the host into the X-Subscription-Token header.
    if !near::agent::host::secret_exists("brave_search_api_key") {
        return Err(
            "Brave Search API key not found in secret store. \
             Set it with: ironclaw secret set brave_search_api_key <key>. \
             Get a free key at: https://brave.com/search/api/"
                .to_string(),
        );
    }

    let kind = match p.action.as_str() {
        "web_search" => "web",
        "news_search" => "news",
        other => {
            return Err(format!(
                "Unknown action '{other}': must be 'web_search' or 'news_search'"
            ));
        }
    };

    search(kind, &p.query, p.count, p.country.as_deref(), p.freshness.as_deref())
}

fn search(
    kind: &str,
    query: &str,
    count: Option<u8>,
    country: Option<&str>,
    freshness: Option<&str>,
) -> Result<String, String> {
    if query.trim().is_empty() {
        return Err("Query must not be empty".to_string());
    }
    if query.len() > 400 {
        return Err(format!(
            "Query too long ({} chars); maximum is 400",
            query.len()
        ));
    }

    let count = count.unwrap_or(5).min(20).max(1);

    // Validate optional parameters
    if let Some(c) = country {
        if c.len() != 2 || !c.chars().all(|ch| ch.is_ascii_alphabetic()) {
            return Err(format!(
                "Invalid country code '{}': must be a 2-letter ISO 3166-1 code (e.g. US, GB)",
                c
            ));
        }
    }
    if let Some(f) = freshness {
        let valid = ["pd", "pw", "pm", "py"];
        if !valid.contains(&f) {
            return Err(format!(
                "Invalid freshness '{}': must be one of pd (past day), pw (past week), \
                 pm (past month), py (past year)",
                f
            ));
        }
    }

    let mut url = format!(
        "https://api.search.brave.com/res/v1/{}/search?q={}&count={}",
        kind,
        url_encode_query(query),
        count
    );
    if let Some(c) = country {
        url.push_str(&format!("&country={}", c.to_uppercase()));
    }
    if let Some(f) = freshness {
        url.push_str(&format!("&freshness={}", f));
    }

    // X-Subscription-Token is injected automatically by the host via the
    // `brave_search_api_key` secret and the header credential mapping.
    let headers = serde_json::json!({
        "Accept": "application/json",
        "Accept-Encoding": "gzip",
        "User-Agent": "IronClaw-BraveSearch-Tool/0.1"
    });

    let response = near::agent::host::http_request("GET", &url, &headers.to_string(), None, None)
        .map_err(|e| format!("HTTP request failed: {}", e))?;

    if response.status < 200 || response.status >= 300 {
        let body = String::from_utf8_lossy(&response.body);
        return Err(format!(
            "Brave Search API error {}: {}",
            response.status, body
        ));
    }

    let body_str =
        String::from_utf8(response.body).map_err(|e| format!("Invalid UTF-8 in response: {}", e))?;

    format_results(kind, &body_str)
}

/// Parse Brave Search JSON and format as a numbered markdown list.
fn format_results(kind: &str, json: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("Failed to parse response JSON: {}", e))?;

    let results_key = if kind == "news" { "results" } else { "results" };
    let items_path = if kind == "news" { "news" } else { "web" };

    // Brave web search: body.web.results[]
    // Brave news search: body.news.results[]
    let items = value
        .get(items_path)
        .and_then(|v| v.get(results_key))
        .and_then(|v| v.as_array());

    let items = match items {
        Some(arr) if !arr.is_empty() => arr,
        _ => {
            return Ok(format!(
                "No {} results found for that query.",
                if kind == "news" { "news" } else { "web" }
            ));
        }
    };

    let mut output = String::new();

    for (i, item) in items.iter().enumerate() {
        let title = item
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("(no title)");

        let url = item
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("(no URL)");

        // News items use "description"; web items use "description" from extra_snippets or plain description
        let snippet = item
            .get("description")
            .and_then(|v| v.as_str())
            .or_else(|| {
                item.get("extra_snippets")
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("");

        if snippet.is_empty() {
            output.push_str(&format!("{}. **{}**\n   {}\n\n", i + 1, title, url));
        } else {
            output.push_str(&format!(
                "{}. **{}**\n   {}\n   {}\n\n",
                i + 1,
                title,
                url,
                snippet
            ));
        }
    }

    Ok(output.trim_end().to_string())
}

const SCHEMA: &str = r#"{
    "type": "object",
    "properties": {
        "action":    { "type": "string", "enum": ["web_search", "news_search"], "default": "web_search",
                       "description": "Type of search: web_search for general web results (default), news_search for recent news" },
        "query":     { "type": "string", "description": "Search query (max 400 characters)" },
        "count":     { "type": "integer", "minimum": 1, "maximum": 20, "default": 5,
                       "description": "Number of results to return (1-20)" },
        "country":   { "type": "string",
                       "description": "ISO 3166-1 alpha-2 country code to bias results (e.g. US, GB, DE)" },
        "freshness": { "type": "string", "enum": ["pd", "pw", "pm", "py"],
                       "description": "Filter by age: pd=past day, pw=past week, pm=past month, py=past year" }
    },
    "required": ["query"]
}"#;

export!(BraveSearchTool);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_encode_query_basic() {
        assert_eq!(url_encode_query("hello world"), "hello+world");
        assert_eq!(url_encode_query("foo-bar_123.baz~"), "foo-bar_123.baz~");
        assert_eq!(url_encode_query("foo&bar=baz"), "foo%26bar%3Dbaz");
    }

    #[test]
    fn test_url_encode_query_special_chars() {
        assert_eq!(url_encode_query("Rust programming"), "Rust+programming");
        assert_eq!(url_encode_query("C++ language"), "C%2B%2B+language");
    }

    #[test]
    fn test_format_results_empty() {
        let json = r#"{"web": {"results": []}}"#;
        let result = format_results("web", json).unwrap();
        assert!(result.contains("No web results found"));
    }

    #[test]
    fn test_format_results_web() {
        let json = r#"{
            "web": {
                "results": [
                    {
                        "title": "Rust Programming Language",
                        "url": "https://www.rust-lang.org",
                        "description": "A language empowering everyone to build reliable software."
                    }
                ]
            }
        }"#;
        let result = format_results("web", json).unwrap();
        assert!(result.contains("1. **Rust Programming Language**"));
        assert!(result.contains("https://www.rust-lang.org"));
        assert!(result.contains("empowering everyone"));
    }

    #[test]
    fn test_format_results_news() {
        let json = r#"{
            "news": {
                "results": [
                    {
                        "title": "Rust 2024 Edition Released",
                        "url": "https://blog.rust-lang.org/2024",
                        "description": "The Rust 2024 edition brings many improvements."
                    }
                ]
            }
        }"#;
        let result = format_results("news", json).unwrap();
        assert!(result.contains("1. **Rust 2024 Edition Released**"));
    }

    #[test]
    fn test_format_results_no_snippet() {
        let json = r#"{
            "web": {
                "results": [
                    {
                        "title": "Example",
                        "url": "https://example.com"
                    }
                ]
            }
        }"#;
        let result = format_results("web", json).unwrap();
        assert!(result.contains("1. **Example**"));
        assert!(result.contains("https://example.com"));
    }

    #[test]
    fn test_format_results_invalid_json() {
        assert!(format_results("web", "not json").is_err());
    }

    #[test]
    fn test_schema_is_valid_json() {
        let v: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
        // action should NOT be required (it has a default)
        let required = v["required"].as_array().unwrap();
        let names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(!names.contains(&"action"), "action should not be required");
        assert!(names.contains(&"query"), "query must be required");
    }

    #[test]
    fn test_params_action_defaults_to_web_search() {
        // Simulates the LLM omitting the action field entirely
        let p: BraveSearchParams = serde_json::from_str(r#"{"query":"rust"}"#).unwrap();
        assert_eq!(p.action, "web_search");
        assert_eq!(p.query, "rust");
    }

    #[test]
    fn test_params_explicit_news_action() {
        let p: BraveSearchParams =
            serde_json::from_str(r#"{"action":"news_search","query":"rust"}"#).unwrap();
        assert_eq!(p.action, "news_search");
    }

    #[test]
    fn test_params_missing_query_is_error() {
        let result: Result<BraveSearchParams, _> = serde_json::from_str(r#"{}"#);
        assert!(result.is_err(), "query is required; empty object should fail");
    }
}
