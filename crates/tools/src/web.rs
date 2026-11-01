//! Hermes's `web_search` and `web_extract` (`tools/web_tools*.py`, `plugins/web/keyless_mcp.py`;
//! MIT) on its keyless tier: the public free endpoints of Exa and Parallel (MCP `tools/call` over
//! HTTPS), Firecrawl and Keenable, taken round-robin with failover when one is rate-limited or
//! refuses. No keys and no user identifiers are sent (Parallel gets a random per-run session id).
//!
//! Extracted pages over the character budget come back as a head + tail window; the whole text
//! is saved under the cache folder for `read_file` to page through.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

const EXA_MCP_URL: &str = "https://mcp.exa.ai/mcp";
const PARALLEL_MCP_URL: &str = "https://search.parallel.ai/mcp";
const FIRECRAWL_API_URL: &str = "https://api.firecrawl.dev";
const KEENABLE_API_URL: &str = "https://api.keenable.ai";
const KEENABLE_TITLE: &str = "hermes-agent";
const TIMEOUT: Duration = Duration::from_secs(30);
const FIRECRAWL_TIMEOUT: Duration = Duration::from_secs(60);
/// `DEFAULT_EXTRACT_CHAR_LIMIT` and its clamp.
const DEFAULT_CHAR_LIMIT: usize = 15_000;
const CHAR_LIMIT_FLOOR: usize = 2_000;
const CHAR_LIMIT_CEILING: usize = 500_000;
/// `MAX_STORED_TEXT_CHARS`.
const MAX_STORED_TEXT_CHARS: usize = 2_000_000;
const MAX_EXTRACT_URLS: usize = 5;
const RATE_LIMIT_MARKERS: &[&str] = &[
    "rate limit",
    "rate-limit",
    "ratelimit",
    "too many requests",
    "429",
    "quota exceeded",
    "slow down",
];
/// Cloud metadata hosts are never fetched (`_BLOCKED_HOSTNAMES`).
const BLOCKED_HOSTNAMES: &[&str] = &["metadata.google.internal", "metadata.goog"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Vendor {
    Exa,
    Parallel,
    Firecrawl,
    Keenable,
}

/// `_KEYLESS_RING`.
const RING: [Vendor; 4] = [
    Vendor::Exa,
    Vendor::Parallel,
    Vendor::Firecrawl,
    Vendor::Keenable,
];

impl Vendor {
    fn hint(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Vendor::Exa => ("Exa", "EXA_API_KEY", "https://exa.ai"),
            Vendor::Parallel => ("Parallel", "PARALLEL_API_KEY", "https://parallel.ai"),
            Vendor::Firecrawl => ("Firecrawl", "FIRECRAWL_API_KEY", "https://firecrawl.dev"),
            Vendor::Keenable => ("Keenable", "KEENABLE_API_KEY", "https://keenable.ai"),
        }
    }

    /// `_fail_msg` (Hermes points at `hermes tools`; keyed backends aren't in hermes-rs yet).
    fn failure(self, kind: &str, error: &str) -> String {
        let (label, _, _) = self.hint();
        format!("Keyless {label} {kind} failed: {error}.")
    }
}

/// One search result row (`web_hit`).
fn hit(url: &str, title: &str, description: &str, position: usize) -> Value {
    json!({"url": url, "title": title, "description": description, "position": position})
}

/// One extracted page (`document`) or failure (`page_error`).
#[derive(Debug, Clone, Default)]
struct Page {
    url: String,
    title: String,
    content: String,
    error: Option<String>,
}

impl Page {
    fn error(url: &str, error: impl Into<String>) -> Self {
        Self {
            url: url.to_string(),
            error: Some(error.into()),
            ..Default::default()
        }
    }
}

/// The web tools every session shares (the vendor ring's cursor, the HTTP client).
pub struct Web {
    client: reqwest::Client,
    cursor: AtomicUsize,
    session_id: String,
    /// Where full page texts are saved (`cache/web`).
    cache_dir: PathBuf,
}

fn is_rate_limitish(message: &str) -> bool {
    let lower = message.to_lowercase();
    RATE_LIMIT_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

/// `_is_search_failover_eligible`: rate limits and HTTP 401/402/403/5xx refusals are local to one
/// free endpoint; another vendor may still serve the query.
fn failover_eligible(message: &str) -> bool {
    static REFUSAL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)^\s*(?:keyless\s+\w+\s+search\s+failed:\s*)?(?:http(?:\s+status)?|status(?:\s+code)?|client\s+error|error\s+code)\s*[:=']*\s*(?:40[123]|5\d\d)\b",
        )
        .expect("refusal pattern")
    });
    is_rate_limitish(message) || REFUSAL.is_match(message)
}

/// `_parse_mcp_body`: the first text item of a `tools/call` result, from a JSON body or SSE
/// `data:` lines. JSON-RPC errors and `isError` results are errors.
fn parse_mcp_body(body: &str) -> Result<String, String> {
    let from_payload = |payload: &str| -> Result<Option<String>, String> {
        let payload = payload.trim();
        if !payload.starts_with('{') {
            return Ok(None);
        }
        let Ok(data) = serde_json::from_str::<Value>(payload) else {
            return Ok(None);
        };
        if let Some(error) = data.get("error").filter(|e| !e.is_null()) {
            return Err(error
                .get("message")
                .and_then(Value::as_str)
                .map_or_else(|| error.to_string(), str::to_string));
        }
        let result = data.get("result").cloned().unwrap_or_default();
        let texts: Vec<&str> = result
            .get("content")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|c| c.get("text")?.as_str())
                    .collect()
            })
            .unwrap_or_default();
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            let message = texts.join(" ");
            return Err(if message.trim().is_empty() {
                "MCP tool call failed".into()
            } else {
                message
            });
        }
        Ok(Some(
            texts
                .into_iter()
                .find(|t| !t.is_empty())
                .unwrap_or("")
                .to_string(),
        ))
    };
    let stripped = body.trim();
    let mut candidates: Vec<&str> = Vec::new();
    if stripped.starts_with('{') {
        candidates.push(stripped);
    }
    candidates.extend(
        body.split(['\n', '\r'])
            .filter_map(|line| line.strip_prefix("data: ")),
    );
    let mut envelope_seen = false;
    for candidate in candidates {
        if let Some(text) = from_payload(candidate)? {
            if !text.is_empty() {
                return Ok(text);
            }
            envelope_seen = true;
        }
    }
    Err(if envelope_seen {
        "MCP response contained no text content".into()
    } else {
        "Unrecognized MCP response shape".into()
    })
}

/// `_parse_exa_search_text`: Exa's `---`-separated `Title:/URL:/Highlights:` blocks.
fn parse_exa_search(text: &str, limit: usize) -> Vec<Value> {
    const LABELS: &[&str] = &["Title:", "URL:", "Highlights:", "Published:", "Author:"];
    let mut results = Vec::new();
    for block in text.split("\n---\n") {
        let (mut title, mut url) = (String::new(), String::new());
        let mut highlights: Vec<&str> = Vec::new();
        let mut in_highlights = false;
        for line in block.lines().map(str::trim) {
            if let Some(rest) = line.strip_prefix("Title:") {
                title = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("URL:") {
                url = rest.trim().to_string();
            } else if in_highlights
                && !line.is_empty()
                && !LABELS.iter().any(|l| line.starts_with(l))
            {
                highlights.push(line);
            }
            if LABELS.iter().any(|l| line.starts_with(l)) {
                in_highlights = line.starts_with("Highlights:");
            }
        }
        if !url.is_empty() {
            results.push(hit(&url, &title, &highlights.join(" "), results.len() + 1));
        }
        if limit > 0 && results.len() >= limit {
            break;
        }
    }
    results
}

/// Firecrawl's result list across its response shapes (`_extract_web_search_results`).
fn firecrawl_rows(response: &Value) -> Vec<Value> {
    let data = response.get("data");
    let list = match data {
        Some(Value::Array(items)) => Some(items),
        Some(data) => data
            .get("web")
            .or_else(|| data.get("results"))
            .and_then(Value::as_array),
        None => response
            .get("web")
            .or_else(|| response.get("results"))
            .and_then(Value::as_array),
    };
    list.map(|items| {
        items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let field = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or("");
                let description = [field("description"), field("snippet"), field("markdown")]
                    .into_iter()
                    .find(|s| !s.is_empty())
                    .unwrap_or("");
                hit(field("url"), field("title"), description, i + 1)
            })
            .collect()
    })
    .unwrap_or_default()
}

impl Web {
    pub fn new(cache_dir: PathBuf) -> Self {
        let session_id = format!("{:032x}", uuid::Uuid::new_v4().as_u128());
        let start = usize::from_str_radix(&session_id[..8], 16).unwrap_or(0) % RING.len();
        Self {
            client: reqwest::Client::builder()
                .user_agent("hermes-agent")
                .build()
                .unwrap_or_default(),
            cursor: AtomicUsize::new(start),
            session_id,
            cache_dir,
        }
    }

    /// `_ring_order`: round-robin from the cursor, which advances once per request.
    fn ring_order(&self) -> Vec<Vendor> {
        let start = self.cursor.fetch_add(1, Ordering::Relaxed) % RING.len();
        RING[start..]
            .iter()
            .chain(&RING[..start])
            .copied()
            .collect()
    }

    async fn response_text(response: reqwest::Response) -> String {
        let bytes = response.bytes().await.unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// `mcp_call`: a JSON-RPC `tools/call`, returning the text payload.
    async fn mcp_call(&self, url: &str, tool: &str, arguments: Value) -> Result<String, String> {
        let payload = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": tool, "arguments": arguments},
        });
        let response = self
            .client
            .post(url)
            .header("Accept", "application/json, text/event-stream")
            .json(&payload)
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        let status = response.status();
        let body = Self::response_text(response).await;
        if status.as_u16() >= 400 {
            return Err(format!(
                "HTTP {}: {}",
                status.as_u16(),
                truncate(&body, 300)
            ));
        }
        parse_mcp_body(&body)
    }

    async fn rest(&self, request: reqwest::RequestBuilder) -> Result<Value, String> {
        let response = request
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        let status = response.status();
        let body = Self::response_text(response).await;
        if status.as_u16() >= 400 {
            return Err(format!(
                "HTTP {}: {}",
                status.as_u16(),
                truncate(body.trim(), 300)
            ));
        }
        serde_json::from_str(&body).map_err(|e| format!("unexpected payload: {e}"))
    }

    async fn search_with(
        &self,
        vendor: Vendor,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Value>, String> {
        match vendor {
            Vendor::Exa => {
                let text = self
                    .mcp_call(
                        EXA_MCP_URL,
                        "web_search_exa",
                        json!({"query": query, "numResults": limit.max(1)}),
                    )
                    .await?;
                Ok(parse_exa_search(&text, limit))
            }
            Vendor::Parallel => {
                let text = self
                    .mcp_call(
                        PARALLEL_MCP_URL,
                        "web_search",
                        json!({"objective": query, "search_queries": [query], "session_id": self.session_id}),
                    )
                    .await?;
                let data: Value =
                    serde_json::from_str(&text).map_err(|e| format!("unexpected payload: {e}"))?;
                Ok(data
                    .get("results")
                    .and_then(Value::as_array)
                    .map(|results| {
                        results
                            .iter()
                            .take(limit.max(1))
                            .enumerate()
                            .map(|(i, r)| {
                                let excerpts: Vec<&str> = r
                                    .get("excerpts")
                                    .and_then(Value::as_array)
                                    .map(|e| e.iter().filter_map(Value::as_str).collect())
                                    .unwrap_or_default();
                                hit(
                                    r.get("url").and_then(Value::as_str).unwrap_or(""),
                                    r.get("title").and_then(Value::as_str).unwrap_or(""),
                                    &excerpts.join(" "),
                                    i + 1,
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default())
            }
            Vendor::Firecrawl => {
                let response = self
                    .rest(
                        self.client
                            .post(format!("{FIRECRAWL_API_URL}/v2/search"))
                            .json(&json!({"query": query, "limit": limit}))
                            .timeout(FIRECRAWL_TIMEOUT),
                    )
                    .await?;
                Ok(firecrawl_rows(&response))
            }
            Vendor::Keenable => {
                let response = self
                    .rest(
                        self.client
                            .post(format!("{KEENABLE_API_URL}/v1/search/public"))
                            .header("X-Keenable-Title", KEENABLE_TITLE)
                            .json(&json!({"query": query, "max_results": limit.max(1)}))
                            .timeout(TIMEOUT),
                    )
                    .await?;
                Ok(response
                    .get("results")
                    .and_then(Value::as_array)
                    .map(|results| {
                        results
                            .iter()
                            .enumerate()
                            .map(|(i, r)| {
                                let field =
                                    |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("");
                                let snippet = if field("snippet").is_empty() {
                                    field("description")
                                } else {
                                    field("snippet")
                                };
                                hit(field("url"), field("title"), snippet, i + 1)
                            })
                            .collect()
                    })
                    .unwrap_or_default())
            }
        }
    }

    /// `web_search_tool` on the keyless ring (`search_with_failover`).
    pub async fn search(&self, query: &str, limit: Option<i64>) -> Value {
        let limit = limit.unwrap_or(5).clamp(1, 100) as usize;
        if query.trim().is_empty() {
            return json!({"success": false, "error": "query is required"});
        }
        let order = self.ring_order();
        let mut last_error = String::new();
        for vendor in &order {
            match self.search_with(*vendor, query, limit).await {
                Ok(rows) => return json!({"success": true, "data": {"web": rows}}),
                Err(error) => {
                    let message = vendor.failure("search", &error);
                    if !failover_eligible(&error) {
                        return json!({"success": false, "error": message});
                    }
                    tracing::info!("keyless {vendor:?} search unavailable; failing over");
                    last_error = message;
                }
            }
        }
        let names: Vec<String> = order.iter().map(|v| v.hint().0.to_lowercase()).collect();
        json!({"success": false, "error": format!("{last_error} (all keyless vendors unavailable: {})", names.join(", "))})
    }

    async fn extract_with(&self, vendor: Vendor, url: &str) -> Page {
        let result: Result<Page, String> = async {
            match vendor {
                Vendor::Exa => {
                    let text = self.mcp_call(EXA_MCP_URL, "web_fetch_exa", json!({"urls": [url]})).await?;
                    let title = text
                        .lines()
                        .map(str::trim)
                        .find_map(|l| l.strip_prefix("# ").or_else(|| l.strip_prefix("Title:")))
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    Ok(Page {
                        url: url.into(),
                        title,
                        content: text,
                        error: None,
                    })
                }
                Vendor::Parallel => {
                    let text = self
                        .mcp_call(
                            PARALLEL_MCP_URL,
                            "web_fetch",
                            json!({"urls": [url], "objective": "Full page content", "session_id": self.session_id}),
                        )
                        .await?;
                    let data: Value =
                        serde_json::from_str(&text).map_err(|e| format!("unexpected payload: {e}"))?;
                    if let Some(page) = data.get("results").and_then(Value::as_array).and_then(|r| r.first()) {
                        let field = |k: &str| page.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                        let content = [field("full_content"), field("content")]
                            .into_iter()
                            .find(|c| !c.is_empty())
                            .unwrap_or_else(|| {
                                page.get("excerpts")
                                    .and_then(Value::as_array)
                                    .map(|e| e.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n\n"))
                                    .unwrap_or_default()
                            });
                        return Ok(Page {
                            url: url.into(),
                            title: field("title"),
                            content,
                            error: None,
                        });
                    }
                    let error = data
                        .get("errors")
                        .and_then(Value::as_array)
                        .and_then(|e| e.first())
                        .map(|e| {
                            let status = e.get("http_status_code").map(Value::to_string).unwrap_or_default();
                            let reason = e.get("error_type").and_then(Value::as_str).unwrap_or("error");
                            format!("{reason} {status}").trim().to_string()
                        })
                        .unwrap_or_else(|| "no content returned".into());
                    Ok(Page::error(url, error))
                }
                Vendor::Firecrawl => {
                    let response = self
                        .rest(
                            self.client
                                .post(format!("{FIRECRAWL_API_URL}/v2/scrape"))
                                .json(&json!({"url": url, "formats": ["markdown"]}))
                                .timeout(FIRECRAWL_TIMEOUT),
                        )
                        .await?;
                    let payload = response.get("data").filter(|d| d.is_object()).unwrap_or(&response);
                    let metadata = payload.get("metadata").cloned().unwrap_or_default();
                    let content = payload
                        .get("markdown")
                        .or_else(|| payload.get("html"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let status = metadata.get("statusCode").and_then(Value::as_u64);
                    Ok(Page {
                        url: metadata.get("url").and_then(Value::as_str).unwrap_or(url).to_string(),
                        title: metadata.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
                        content,
                        error: status
                            .filter(|s| *s >= 400)
                            .map(|s| format!("HTTP {s} from origin")),
                    })
                }
                Vendor::Keenable => {
                    let response = self
                        .rest(
                            self.client
                                .get(
                                    url::Url::parse_with_params(
                                        &format!("{KEENABLE_API_URL}/v1/fetch/public"),
                                        &[("url", url)],
                                    )
                                    .map_err(|e| e.to_string())?,
                                )
                                .header("X-Keenable-Title", KEENABLE_TITLE)
                                .timeout(TIMEOUT),
                        )
                        .await?;
                    let field = |k: &str| response.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    Ok(Page {
                        url: Some(field("url")).filter(|u| !u.is_empty()).unwrap_or_else(|| url.into()),
                        title: field("title"),
                        content: field("content"),
                        error: None,
                    })
                }
            }
        }
        .await;
        result.unwrap_or_else(|error| Page::error(url, vendor.failure("extract", &error)))
    }

    /// `extract_with_failover`: the next vendor only when every URL came back rate-limited.
    async fn extract_ring(&self, urls: &[String]) -> Vec<Page> {
        let order = self.ring_order();
        let mut pages = Vec::new();
        for vendor in order {
            pages = Vec::new();
            for url in urls {
                pages.push(self.extract_with(vendor, url).await);
            }
            let throttled = pages.iter().all(|page| {
                page.error
                    .as_deref()
                    .is_some_and(|e| !e.contains("from origin") && is_rate_limitish(e))
            });
            if !throttled {
                break;
            }
        }
        pages
    }

    /// `web_extract_tool`: up to five URLs, secrets and private addresses refused, each page's
    /// text within the budget.
    pub async fn extract(&self, urls: &[Value], char_limit: Option<i64>) -> Value {
        let char_limit = char_limit.map_or(DEFAULT_CHAR_LIMIT, |n| {
            (n.max(0) as usize).clamp(CHAR_LIMIT_FLOOR, CHAR_LIMIT_CEILING)
        });
        let mut entries: Vec<Option<Page>> = Vec::new();
        let mut to_fetch: Vec<(usize, String)> = Vec::new();
        for (index, item) in urls.iter().take(MAX_EXTRACT_URLS).enumerate() {
            let url = match item {
                Value::String(s) => Some(s.trim().to_string()),
                Value::Object(o) => o
                    .get("url")
                    .and_then(Value::as_str)
                    .map(|s| s.trim().to_string()),
                _ => None,
            };
            let Some(url) = url.filter(|u| !u.is_empty()) else {
                entries.push(Some(Page::error(
                    "",
                    format!("Item {index} is not a URL string."),
                )));
                continue;
            };
            let url = if url.contains("://") {
                url
            } else {
                format!("https://{url}")
            };
            // URLs carrying secrets are refused outright (exfiltration).
            let decoded = percent_decode(&url);
            if crate::redact::redact(&url, crate::redact::Source::Output) != url
                || crate::redact::redact(&decoded, crate::redact::Source::Output) != decoded
            {
                return json!({"success": false, "error": "Blocked: URL contains what appears to be an API key or token. Secrets must not be sent in URLs."});
            }
            if !is_safe_url(&url).await {
                entries.push(Some(Page::error(
                    &url,
                    "Blocked: URL targets a private or internal network address",
                )));
                continue;
            }
            to_fetch.push((entries.len(), url));
            entries.push(None);
        }
        let fetched = self
            .extract_ring(&to_fetch.iter().map(|(_, u)| u.clone()).collect::<Vec<_>>())
            .await;
        for ((index, _), page) in to_fetch.iter().zip(fetched) {
            entries[*index] = Some(page);
        }
        let results: Vec<Value> = entries
            .into_iter()
            .flatten()
            .map(|mut page| {
                if page.error.is_none() && !page.content.is_empty() {
                    let clean = convert_base64_images(&page.content);
                    page.content = self.truncate_with_footer(&clean, &page.url, char_limit);
                } else if !page.content.is_empty() {
                    page.content = self.truncate_with_footer(&convert_base64_images(&page.content), &page.url, char_limit);
                }
                json!({"url": page.url, "title": page.title, "content": page.content, "error": page.error})
            })
            .collect();
        if results
            .iter()
            .all(|r| r["content"].as_str().unwrap_or("").is_empty() && r["error"].is_null())
        {
            return json!({"success": false, "error": "Content was inaccessible or not found"});
        }
        json!({"results": results})
    }

    /// `_store_full_text`: the whole page under `cache/web`, for `read_file`.
    fn store_full_text(&self, url: &str, content: &str) -> Option<PathBuf> {
        std::fs::create_dir_all(&self.cache_dir).ok()?;
        let host: String = url
            .split("://")
            .nth(1)
            .unwrap_or(url)
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("page")
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .take(60)
            .collect();
        // FNV-1a of the URL keeps the name stable for re-extraction.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in url.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        let path = self
            .cache_dir
            .join(format!("{host}-{:010x}.md", hash & 0xff_ffff_ffff));
        let total = content.chars().count();
        let stored = if total > MAX_STORED_TEXT_CHARS {
            format!(
                "{}\n\n[... stored copy truncated at {} chars of {}; re-extract a more specific URL for the rest ...]",
                content
                    .chars()
                    .take(MAX_STORED_TEXT_CHARS)
                    .collect::<String>(),
                thousands(MAX_STORED_TEXT_CHARS),
                thousands(total)
            )
        } else {
            content.to_string()
        };
        std::fs::write(&path, stored).ok()?;
        Some(path)
    }

    /// `_truncate_with_footer`: a ~75% head / ~25% tail window cut on line boundaries, plus a
    /// footer saying where the full text is and how to read the omitted middle.
    fn truncate_with_footer(&self, content: &str, url: &str, char_limit: usize) -> String {
        let chars: Vec<char> = content.chars().collect();
        if chars.len() <= char_limit {
            return content.to_string();
        }
        let head_budget = char_limit * 3 / 4;
        let tail_budget = char_limit - head_budget;
        let mut head: String = chars[..head_budget].iter().collect();
        let mut tail: String = chars[chars.len() - tail_budget..].iter().collect();
        if let Some(newline) = head.rfind('\n')
            && newline > head_budget / 2
        {
            head.truncate(newline);
        }
        if let Some(newline) = tail.find('\n')
            && newline < tail_budget / 2
        {
            tail = tail[newline + 1..].to_string();
        }
        let rule = "─".repeat(8);
        let mut footer = vec![
            String::new(),
            format!("{rule} [TRUNCATED] {rule}"),
            format!(
                "Showing {} chars (head) + {} chars (tail) of {} total clean characters.",
                thousands(head.chars().count()),
                thousands(tail.chars().count()),
                thousands(chars.len())
            ),
        ];
        match self.store_full_text(url, content) {
            Some(path) => {
                let path = crate::display_path(&path);
                let middle_start = head.matches('\n').count() + 2;
                footer.push(format!("Full text saved to: {path}"));
                footer.push(format!(
                    "To read the omitted middle: read_file path=\"{path}\" offset={middle_start} limit=200  (the file is the complete page; raise/lower offset to page through it)."
                ));
            }
            None => footer.push(
                "Full text could not be stored; re-run web_extract on a more specific URL for the complete page.".into(),
            ),
        }
        footer.push("─".repeat(29));
        format!(
            "{head}\n\n[... middle omitted — see footer ...]\n\n{tail}\n{}",
            footer.join("\n")
        )
    }
}

/// The text of a page for an `@url:` reference: the extracted content (no truncation footer),
/// or why there is none.
impl Web {
    pub async fn fetch_for_reference(&self, url: &str) -> Result<String, String> {
        let url = &if url.contains("://") {
            url.to_string()
        } else {
            format!("https://{url}")
        };
        if !is_safe_url(url).await {
            return Err("URL targets a private or internal network address".into());
        }
        let pages = self.extract_ring(&[url.to_string()]).await;
        match pages.into_iter().next() {
            Some(Page {
                error: Some(error), ..
            }) => Err(error),
            Some(page) if !page.content.trim().is_empty() => {
                Ok(convert_base64_images(&page.content))
            }
            _ => Err("no content extracted".into()),
        }
    }
}

/// `convert_base64_images_to_links`: inline base64 images (token bombs) become `[IMAGE: alt]`.
fn convert_base64_images(text: &str) -> String {
    static MARKDOWN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"!\[(?P<alt>[^\]]*)\]\(\s*data:image/[^;]+;base64,[A-Za-z0-9+/=\s]+\)")
            .expect("pattern")
    });
    static PAREN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\(\s*data:image/[^;]+;base64,[A-Za-z0-9+/=\s]+\)").expect("pattern")
    });
    static BARE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"data:image/[^;]+;base64,[A-Za-z0-9+/=]+").expect("pattern")
    });
    let out = MARKDOWN.replace_all(text, |caps: &regex::Captures<'_>| {
        let alt = caps.name("alt").map_or("", |m| m.as_str()).trim();
        if alt.is_empty() {
            "[IMAGE]".to_string()
        } else {
            format!("[IMAGE: {alt}]")
        }
    });
    let out = PAREN.replace_all(&out, "[IMAGE]");
    BARE.replace_all(&out, "[IMAGE]").into_owned()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&text[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Whether an address is private, loopback, link-local or otherwise internal.
fn is_internal(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64
                || v4.octets()[0] == 0
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| is_internal(IpAddr::V4(v4)))
        }
    }
}

/// `is_safe_url`: http(s), no userinfo, and every address the host resolves to is public. Fails
/// closed on a DNS error.
pub async fn is_safe_url(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_lowercase();
    if BLOCKED_HOSTNAMES.contains(&host.as_str())
        || host == "localhost"
        || host.ends_with(".localhost")
    {
        return false;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return !is_internal(ip);
    }
    let port = url.port_or_known_default().unwrap_or(443);
    match tokio::net::lookup_host((host.as_str(), port)).await {
        Ok(addresses) => {
            let addresses: Vec<_> = addresses.collect();
            !addresses.is_empty() && addresses.iter().all(|a| !is_internal(a.ip()))
        }
        Err(_) => false,
    }
}

/// The `cache/web` folder under the data folder.
pub fn cache_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("cache").join("web")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mcp_bodies() {
        let sse = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hello\"}]}}\n\n";
        assert_eq!(parse_mcp_body(sse).unwrap(), "hello");
        let error = r#"{"jsonrpc":"2.0","id":1,"result":{"isError":true,"content":[{"type":"text","text":"Rate limit exceeded"}]}}"#;
        assert_eq!(parse_mcp_body(error).unwrap_err(), "Rate limit exceeded");
        assert!(parse_mcp_body("<html>").is_err());
    }

    #[test]
    fn parses_exa_results() {
        let text = "Title: Rust\nURL: https://rust-lang.org\nPublished: 2024\nHighlights:\nA language\nempowering everyone\n---\nTitle: Book\nURL: https://doc.rust-lang.org/book\nHighlights:\nThe book";
        let rows = parse_exa_search(text, 5);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["url"], "https://rust-lang.org");
        assert_eq!(rows[0]["description"], "A language empowering everyone");
        assert_eq!(rows[1]["position"], 2);
        assert_eq!(parse_exa_search(text, 1).len(), 1);
    }

    #[test]
    fn failover_and_images() {
        assert!(failover_eligible("HTTP 429: slow down"));
        assert!(failover_eligible("HTTP 503: unavailable"));
        assert!(!failover_eligible("HTTP 400: invalid query 'http 503'"));
        assert_eq!(
            convert_base64_images(
                "see ![logo](data:image/png;base64,AAAA) and data:image/gif;base64,BBB"
            ),
            "see [IMAGE: logo] and [IMAGE]"
        );
    }

    #[tokio::test]
    async fn private_addresses_are_blocked() {
        assert!(!is_safe_url("http://127.0.0.1/admin").await);
        assert!(!is_safe_url("http://192.168.1.1").await);
        assert!(!is_safe_url("http://[::1]/").await);
        assert!(!is_safe_url("http://localhost:8080").await);
        assert!(!is_safe_url("http://169.254.169.254/latest").await);
        assert!(!is_safe_url("file:///C:/Windows").await);
        assert!(!is_safe_url("https://user:pass@example.com").await);
        assert!(is_safe_url("https://93.184.215.14/").await);
    }

    /// The real keyless ring: `cargo test -p hermes_tools live_web -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn live_web_search_and_extract() {
        let dir = tempfile::tempdir().unwrap();
        let web = Web::new(dir.path().to_path_buf());
        for _ in 0..RING.len() {
            let found = web.search("rust programming language", Some(3)).await;
            println!("search: {found}");
            assert_eq!(found["success"], true, "{found}");
        }
        let page = web.extract(&[json!("https://example.com")], None).await;
        println!("extract: {page}");
        assert!(
            page["results"][0]["content"]
                .as_str()
                .unwrap()
                .contains("documentation examples")
        );
    }

    #[tokio::test]
    async fn long_pages_get_a_window_and_a_saved_copy() {
        let dir = tempfile::tempdir().unwrap();
        let web = Web::new(dir.path().to_path_buf());
        let page: String = (0..3000).map(|i| format!("line {i}\n")).collect();
        let text = web.truncate_with_footer(&page, "https://example.com/a", 2_000);
        assert!(text.starts_with("line 0\n"));
        assert!(text.contains("[... middle omitted — see footer ...]"));
        assert!(text.contains("of 28,890 total clean characters."), "{text}");
        let saved = text
            .split("Full text saved to: ")
            .nth(1)
            .unwrap()
            .lines()
            .next()
            .unwrap();
        assert_eq!(std::fs::read_to_string(saved).unwrap(), page);
        let refused = web
            .extract(
                &[json!(
                    "https://example.com/?key=sk-abcdefghijklmnopqrstuvwx1234"
                )],
                None,
            )
            .await;
        assert!(refused["error"].as_str().unwrap().contains("API key"));
    }
}
