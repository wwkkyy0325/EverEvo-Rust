//! `wayback_lookup` — Internet Archive Wayback Machine snapshot discovery + fetch.
//!
//! Dead-source pre-routing for benchmark questions that reference HISTORICAL
//! pages no longer live (BASE 633 "as of 2020", arXiv 2020-01 hep-lat list,
//! ScienceDirect subject browse, Met zodiac object page). The live page has
//! changed or is blocked; the archived snapshot still holds the answer.
//!
//! Actions:
//! - `list` (default): query the CDX index for 200-status captures in a date
//!   range and return snapshot URLs (`web.archive.org/web/{ts}/{url}`).
//! - `raw`: fetch a snapshot's RAW content (`{ts}id_` — no Wayback toolbar) for
//!   the given timestamp, or the closest capture to `to` via the Availability
//!   API when no timestamp is given.
//!
//! Gotchas handled (2026-08-14 web research): `output=json` required (default
//! CDX is space-separated); skip row 0 (headers); `exact` match is safe while
//! `host/domain` may 403 on popular URLs; sporadic 503s → retry 2-5s backoff;
//! courtesy rate-limit ~250ms between calls.

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

/// CDX search endpoint.
const CDX_URL: &str = "https://web.archive.org/cdx/search/cdx";
/// Availability API (closest snapshot to a date).
const AVAIL_URL: &str = "https://archive.org/wayback/available";
/// Max raw content chars returned.
const MAX_RAW_CHARS: usize = 40_000;

pub struct WaybackLookupTool;

impl WaybackLookupTool {
    pub fn new() -> Self {
        Self
    }

    async fn client() -> Result<reqwest::Client, EverEvoError> {
        // 25s per call; 3 attempts with backoff stay inside the loop's ~120s
        // per-tool budget even when the CDX index is slow.
        everevo_net::reqwest_apply_proxy(reqwest::Client::builder())
            .timeout(std::time::Duration::from_secs(25))
            .build()
            .map_err(|e| EverEvoError::Internal(format!("wayback: http client: {e}")))
    }

    /// GET with 503/transient retry (2-5s backoff, 3 attempts).
    async fn get_retry(client: &reqwest::Client, url: &str) -> Result<String, EverEvoError> {
        let mut last = String::from("unknown");
        for attempt in 0..3 {
            match client.get(url).send().await {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if status == 503 || status == 429 || status >= 500 {
                        last = format!("HTTP {status}");
                        tokio::time::sleep(std::time::Duration::from_secs(2 + attempt as u64))
                            .await;
                        continue;
                    }
                    let text = resp
                        .text()
                        .await
                        .map_err(|e| EverEvoError::Internal(format!("wayback: read: {e}")))?;
                    if status != 200 {
                        return Err(EverEvoError::Internal(format!(
                            "wayback: HTTP {status} for {url}"
                        )));
                    }
                    return Ok(text);
                }
                Err(e) => {
                    last = format!("{e}");
                    tokio::time::sleep(std::time::Duration::from_secs(2 + attempt as u64)).await;
                }
            }
        }
        Err(EverEvoError::Internal(format!(
            "wayback: retries exhausted ({last})"
        )))
    }

    /// CDX list → snapshot URLs (200-status captures, deduped per day).
    async fn list_snapshots(url: &str, from: &str, to: &str) -> Result<String, EverEvoError> {
        let mut q = format!(
            "{CDX_URL}?url={url}&output=json&filter=statuscode:200&collapse=timestamp:8&fl=timestamp,original,statuscode"
        );
        if !from.is_empty() {
            q.push_str(&format!("&from={from}"));
        }
        if !to.is_empty() {
            q.push_str(&format!("&to={to}"));
        }
        let client = Self::client().await?;
        let text = Self::get_retry(&client, &q).await?;
        let arr: Vec<Vec<String>> = serde_json::from_str(&text).unwrap_or_default();
        // Row 0 is the header; skip it.
        let rows = arr.into_iter().skip(1).collect::<Vec<_>>();
        if rows.is_empty() {
            return Ok(format!(
                "No archived snapshots found for {url} (from={} to={}).",
                if from.is_empty() { "any" } else { from },
                if to.is_empty() { "any" } else { to }
            ));
        }
        let mut out = format!("## Wayback snapshots for {url}\n\n");
        for (i, row) in rows.iter().enumerate() {
            let ts = row.first().cloned().unwrap_or_default();
            let orig = row.get(1).cloned().unwrap_or_else(|| url.to_string());
            let status = row.get(2).cloned().unwrap_or_default();
            out.push_str(&format!(
                "{}. `web.archive.org/web/{ts}/{orig}`  (captured {ts}, status {status})\n",
                i + 1
            ));
        }
        out.push_str("\nFetch a snapshot's RAW content with action=raw + timestamp=<ts>.");
        Ok(out)
    }

    /// Raw content of a snapshot: use `{ts}id_` (no toolbar). When `ts` is
    /// empty, resolve the closest capture to `to` (or now) via Availability.
    async fn raw_snapshot(url: &str, ts: &str, to: &str) -> Result<String, EverEvoError> {
        let client = Self::client().await?;
        let (target, body) = if ts.is_empty() {
            // Availability API → closest snapshot.
            let ts_param = if to.is_empty() {
                "now".to_string()
            } else {
                to.to_string()
            };
            let avail = format!("{AVAIL_URL}?url={url}&timestamp={ts_param}");
            let text = Self::get_retry(&client, &avail).await?;
            let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            let snap = &v["archived_snapshots"]["closest"];
            if snap["status"].as_str() != Some("200") {
                return Ok(format!(
                    "No 200 snapshot of {url} near {ts_param} found via Availability API."
                ));
            }
            let s_url = snap["url"].as_str().unwrap_or("").to_string();
            let fetched = Self::get_retry(&client, &s_url).await?;
            (s_url, fetched)
        } else {
            let raw_url = format!("https://web.archive.org/web/{ts}id_/{url}");
            let fetched = Self::get_retry(&client, &raw_url).await?;
            (raw_url, fetched)
        };
        let truncated: String = body.chars().take(MAX_RAW_CHARS).collect();
        let note = if body.chars().count() > MAX_RAW_CHARS {
            format!(
                "\n... (truncated, {} of {} chars)",
                MAX_RAW_CHARS,
                body.chars().count()
            )
        } else {
            String::new()
        };
        Ok(format!("## Raw content of {target}\n\n{truncated}{note}"))
    }
}

impl Default for WaybackLookupTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WaybackLookupTool {
    fn name(&self) -> &str {
        "wayback_lookup"
    }

    fn description(&self) -> &str {
        "Find or fetch Internet Archive Wayback Machine snapshots of a URL. \
         Use when a question references a HISTORICAL page that may no longer \
         exist live (e.g. 'as of 2020', an old arXiv listing, a dead science \
         site). Actions: list (default — archived snapshot URLs in a date \
         range), raw (fetch a snapshot's RAW content). Parameters: url \
         (required), from / to (optional YYYYMMDD date range, partial like \
         '2020' works), action (list|raw), timestamp (optional 14-digit capture \
         time for raw)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "Original URL to look up"},
                "from": {"type": "string", "description": "Start date (YYYYMMDD or partial YYYY)"},
                "to": {"type": "string", "description": "End date (YYYYMMDD or partial YYYY)"},
                "action": {"type": "string", "enum": ["list", "raw"], "default": "list",
                           "description": "list = snapshot URLs; raw = fetch a snapshot's raw content"},
                "timestamp": {"type": "string",
                              "description": "14-digit capture time (YYYYMMDDhhmmss) for action=raw"}
            },
            "required": ["url"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        let url = params["url"].as_str().unwrap_or("").trim().to_string();
        if url.is_empty() {
            return Ok(ToolOutput {
                content: "url is required".into(),
                is_error: true,
                ..Default::default()
            });
        }
        let action = params["action"].as_str().unwrap_or("list");
        let from = params["from"].as_str().unwrap_or("");
        let to = params["to"].as_str().unwrap_or("");
        let ts = params["timestamp"].as_str().unwrap_or("");

        let out = match action {
            "raw" => Self::raw_snapshot(&url, ts, to).await?,
            _ => Self::list_snapshots(&url, from, to).await?,
        };
        Ok(ToolOutput {
            content: out,
            is_error: false,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_schema() {
        let t = WaybackLookupTool::new();
        assert_eq!(t.name(), "wayback_lookup");
        let s = t.parameters_schema();
        assert_eq!(s["required"][0], "url");
        assert!(s["properties"]["action"]["enum"].is_array());
    }

    #[tokio::test]
    async fn missing_url_is_error() {
        let t = WaybackLookupTool::new();
        let out = t.execute(serde_json::json!({}), None).await.unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("url is required"));
    }
}
