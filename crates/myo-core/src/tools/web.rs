//! The `web_search` tool — keyless web search, configurable backend.
//!
//! Two backends ([`WebSearchConfig`]): DuckDuckGo's keyless HTML endpoint
//! (the default — works anywhere, no API key) and a self-hosted SearXNG
//! instance's JSON API (cleaner results when you run one). Both return a small
//! list of [`Hit`]s the tool formats as plain text for the model.
//!
//! The DDG path scrapes HTML, which is inherently a bit brittle; parsing is
//! isolated in [`parse_ddg`] (with fixture tests) so tracking a markup change is
//! a one-function edit, and SearXNG is the robust alternative when that matters.

use std::time::Duration;

use anyhow::{anyhow, Result};
use reqwest::Client;
use serde_json::{json, Value};

use crate::config::WebSearchConfig;

use super::{Category, Tool, ToolCtx, ToolResult};

/// One search result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Shared web-search client — one per app, holding the backend choice and a
/// reqwest client with sane timeouts and a browser-ish UA (DDG is unhappy
/// without one).
pub struct WebSearch {
    http: Client,
    config: WebSearchConfig,
}

impl WebSearch {
    pub fn new(config: WebSearchConfig) -> Result<Self> {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent("Mozilla/5.0 (X11; Linux x86_64; rv:121.0) Gecko/20100101 Firefox/121.0")
            .build()?;
        Ok(Self { http, config })
    }

    /// Run a search, returning up to `limit` hits.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<Hit>> {
        match &self.config {
            WebSearchConfig::Ddg => self.search_ddg(query, limit).await,
            WebSearchConfig::Searxng { url } => self.search_searxng(url, query, limit).await,
        }
    }

    async fn search_ddg(&self, query: &str, limit: usize) -> Result<Vec<Hit>> {
        let resp = self
            .http
            .get("https://html.duckduckgo.com/html/")
            .query(&[("q", query)])
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!("web search failed (HTTP {})", resp.status()));
        }
        let html = resp.text().await?;
        Ok(parse_ddg(&html, limit))
    }

    async fn search_searxng(&self, base: &str, query: &str, limit: usize) -> Result<Vec<Hit>> {
        let url = format!("{}/search", base.trim_end_matches('/'));
        let resp = self
            .http
            .get(&url)
            .query(&[("q", query), ("format", "json")])
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!("web search failed (HTTP {})", resp.status()));
        }
        let v: Value = resp.json().await?;
        Ok(parse_searxng(&v, limit))
    }
}

/// Pull hits out of a SearXNG JSON response (`results[]` of `{title,url,content}`).
fn parse_searxng(v: &Value, limit: usize) -> Vec<Hit> {
    v.get("results")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let url = r.get("url").and_then(Value::as_str)?.to_string();
                    let title = r
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let snippet = r
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    Some(Hit {
                        title,
                        url,
                        snippet,
                    })
                })
                .take(limit)
                .collect()
        })
        .unwrap_or_default()
}

/// Scrape hits out of a DuckDuckGo HTML results page. The stable markers are the
/// `result__a` result anchor (title + a `uddg=`-wrapped redirect href) and the
/// `result__snippet` element. Tolerant: anything it can't parse is skipped.
fn parse_ddg(html: &str, limit: usize) -> Vec<Hit> {
    let mut hits = Vec::new();
    // Walk each result anchor. The class attribute can carry extra classes, so we
    // match on the marker substring rather than an exact attribute value.
    let mut rest = html;
    while let Some(pos) = rest.find("result__a") {
        rest = &rest[pos..];
        // href="..." appears just before the class on the same <a>; search a small
        // window backwards isn't simple on &str, so instead grab the href that
        // follows within this anchor's tag by scanning forward from the tag start.
        // The anchor opens at the nearest '<' before our marker.
        let tag_start = html[..html.len() - rest.len()].rfind('<').unwrap_or(0);
        let after_tag = &html[tag_start..];
        let href = extract_attr(after_tag, "href=\"");
        let title_raw = extract_between(after_tag, ">", "</a>");
        // Advance past this anchor so the loop makes progress.
        rest = &rest["result__a".len()..];

        let (Some(href), Some(title_raw)) = (href, title_raw) else {
            continue;
        };
        let url = decode_ddg_href(&href);
        if url.is_empty() {
            continue;
        }
        let title = clean(&title_raw);
        // The snippet follows the anchor; look for the next snippet marker in the
        // remaining tail and pull its text.
        let snippet = find_after(after_tag, "result__snippet")
            .and_then(|tail| {
                extract_between(tail, ">", "</a>").or_else(|| extract_between(tail, ">", "</div>"))
            })
            .map(|s| clean(&s))
            .unwrap_or_default();

        hits.push(Hit {
            title,
            url,
            snippet,
        });
        if hits.len() >= limit {
            break;
        }
    }
    hits
}

/// The value of `attr` (e.g. `href="`) in `s`, up to the closing quote.
fn extract_attr(s: &str, attr: &str) -> Option<String> {
    let start = s.find(attr)? + attr.len();
    let tail = &s[start..];
    let end = tail.find('"')?;
    Some(tail[..end].to_string())
}

/// The text between the first `open` and the following `close` in `s`.
fn extract_between(s: &str, open: &str, close: &str) -> Option<String> {
    let start = s.find(open)? + open.len();
    let tail = &s[start..];
    let end = tail.find(close)?;
    Some(tail[..end].to_string())
}

/// The slice of `s` starting at the first occurrence of `marker` (inclusive),
/// for chained extraction.
fn find_after<'a>(s: &'a str, marker: &str) -> Option<&'a str> {
    s.find(marker).map(|p| &s[p..])
}

/// Turn a DDG result href into a real URL. DDG wraps targets as
/// `//duckduckgo.com/l/?uddg=<percent-encoded-url>&...`; pull and decode it.
/// A non-wrapped absolute href is returned as-is.
fn decode_ddg_href(href: &str) -> String {
    if let Some(idx) = href.find("uddg=") {
        let tail = &href[idx + "uddg=".len()..];
        let enc = tail.split('&').next().unwrap_or(tail);
        return percent_decode(enc);
    }
    if href.starts_with("//") {
        format!("https:{href}")
    } else {
        href.to_string()
    }
}

/// Minimal percent-decoding (`%XX` → byte, `+` → space), enough for the `uddg`
/// target. Invalid escapes are left verbatim.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Strip HTML tags and decode the handful of entities DDG emits, then trim.
fn clean(s: &str) -> String {
    let mut no_tags = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => no_tags.push(c),
            _ => {}
        }
    }
    no_tags
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .trim()
        .to_string()
}

pub struct WebSearchTool;

#[async_trait::async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn category(&self) -> Category {
        Category::Web
    }

    fn headline(&self, args: &Value) -> Option<String> {
        args.get("query")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the web and get back a list of results (title, URL, and a \
                                snippet). Use it to look up current facts, find pages, or gather \
                                sources before answering.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "The search query." },
                        "limit": {
                            "type": "integer",
                            "description": "Max results to return (default 5)."
                        }
                    },
                    "required": ["query"]
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("web_search requires a 'query' string"))?
            .to_string();
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n.clamp(1, 10) as usize)
            .unwrap_or(5);

        ctx.progress(self.name(), format!("Searching the web for: {query}"));
        let hits = ctx.web.search(&query, limit).await?;
        if hits.is_empty() {
            return Ok(ToolResult::text(format!(
                "No web results found for \"{query}\"."
            )));
        }
        let mut text = format!("Top {} web results for \"{query}\":\n", hits.len());
        for (i, h) in hits.iter().enumerate() {
            text.push_str(&format!(
                "\n{}. {}\n{}\n{}\n",
                i + 1,
                if h.title.is_empty() {
                    "(untitled)"
                } else {
                    &h.title
                },
                h.url,
                h.snippet
            ));
        }
        Ok(ToolResult::text(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_basics() {
        assert_eq!(percent_decode("https%3A%2F%2Fa.com%2Fx"), "https://a.com/x");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[test]
    fn clean_strips_tags_and_entities() {
        assert_eq!(clean("<b>Rust</b> &amp; you"), "Rust & you");
        assert_eq!(clean("  spaced &#x27;quote&#x27;  "), "spaced 'quote'");
    }

    #[test]
    fn decode_ddg_href_unwraps_uddg() {
        let href = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2F&rut=abc";
        assert_eq!(decode_ddg_href(href), "https://www.rust-lang.org/");
        assert_eq!(decode_ddg_href("//example.com/x"), "https://example.com/x");
    }

    #[test]
    fn parse_ddg_extracts_hits_from_fixture() {
        // A trimmed shape of DDG's HTML results markup.
        let html = r#"
        <div class="result results_links">
          <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2F">The <b>Rust</b> Language</a>
          <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2F">A language empowering everyone.</a>
        </div>
        <div class="result results_links">
          <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdoc.rust-lang.org%2F">Rust Docs</a>
          <a class="result__snippet" href="x">The official docs &amp; book.</a>
        </div>
        "#;
        let hits = parse_ddg(html, 5);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://www.rust-lang.org/");
        assert_eq!(hits[0].title, "The Rust Language");
        assert_eq!(hits[0].snippet, "A language empowering everyone.");
        assert_eq!(hits[1].url, "https://doc.rust-lang.org/");
        assert_eq!(hits[1].snippet, "The official docs & book.");
    }

    #[test]
    fn parse_ddg_respects_limit() {
        let html = r#"
          <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.com">A</a>
          <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fb.com">B</a>
          <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fc.com">C</a>
        "#;
        assert_eq!(parse_ddg(html, 2).len(), 2);
    }

    #[test]
    fn parse_searxng_reads_results() {
        let v = json!({
            "results": [
                { "title": "A", "url": "https://a.com", "content": "first" },
                { "title": "B", "url": "https://b.com", "content": "second" }
            ]
        });
        let hits = parse_searxng(&v, 5);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[1].url, "https://b.com");
        assert_eq!(hits[0].snippet, "first");
    }
}
