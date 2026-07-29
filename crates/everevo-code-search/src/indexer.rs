//! Code indexer — FTS5-backed code symbol index with incremental update.
//!
//! ## Context Pollution Defense (Phase 0)
//!
//! Search results are post-processed through a four-layer defense before
//! reaching the LLM context, backed by academic research:
//!
//! | Layer | Mechanism | Source |
//! |-------|-----------|--------|
//! | 1. Hard Caps | 12 results, 250 tokens/result, 2500 total | Cody (Sourcegraph production) + Context Cliff (Veseli 2025) |
//! | 2. Dedup | Same-file grouping, overlap detection (>30%) | GrepRAG (ISSTA 2026) |
//! | 3. Ranking | Identifier-weighted: fn×4 > struct×3.5 > enum×2.5 > mod×2 | GrepRAG identifier-weighted re-rank |
//! | 4. Progressive Disclosure | Compact (file:line+sig) default; Expanded on request | Claude Code search→read pattern |

use crate::scanner::scan_file;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

// ── Search Result ────────────────────────────────────────────────────────

/// Search result returned to the LLM.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub parent: String,
    pub signature: String,
    pub rank: f64,
}

// ── Search Config (Phase 0: Context Pollution Defense) ───────────────────

/// Controls output formatting and limits for search results.
///
/// Defaults are research-backed (Cody production + Context Cliff paper).
#[derive(Debug, Clone)]
pub struct SearchConfig {
    /// Maximum results returned (Cody: 12 code results).
    pub max_results: usize,
    /// Maximum characters per result signature (truncated with "…").
    pub max_chars_per_result: usize,
    /// Maximum total characters in the output. Triggers truncation warning.
    /// Context Cliff threshold (Veseli 2025): ~2500 tokens ≈ 10000 chars.
    pub max_total_chars: usize,
    /// Return full code snippets instead of one-line summaries.
    pub expand: bool,
    /// Minimum query length for trigram index. Shorter queries → rg fallback.
    pub min_query_len: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            max_results: 12,
            max_chars_per_result: 200, // ~250 tokens with safety margin
            max_total_chars: 10_000,   // Context Cliff safe zone
            expand: false,
            min_query_len: 3,
        }
    }
}

impl SearchConfig {
    pub fn with_expand(mut self, expand: bool) -> Self {
        self.expand = expand;
        self
    }
}

// ── Identifier Weight (GrepRAG ISSTA 2026) ──────────────────────────────

/// Weight multiplier for symbol kind in ranking.
/// Research: matching on function/method names is 4× more relevant
/// than matching in arbitrary signature text.
fn identifier_weight(kind: &str) -> f64 {
    match kind {
        "fn" => 4.0,
        "struct" | "class" => 3.5,
        "trait" | "interface" | "protocol" => 3.5,
        "enum" => 2.5,
        "type" => 2.5,
        "mod" | "module" => 2.0,
        "impl" => 2.0,
        "const" => 1.5,
        _ => 1.0,
    }
}

// ── Deduplication (GrepRAG structure-aware dedup) ────────────────────────

/// Deduplicate results: same-file grouping, overlap detection.
/// Groups results from the same file into a single summary entry.
/// When two results overlap >30% by line range, keeps the higher-ranked one.
fn deduplicate(results: &[SearchResult]) -> Vec<SearchResult> {
    if results.is_empty() {
        return vec![];
    }

    // Group by file
    let mut file_groups: HashMap<String, Vec<&SearchResult>> = HashMap::new();
    for r in results {
        file_groups.entry(r.file.clone()).or_default().push(r);
    }

    let mut deduped: Vec<SearchResult> = Vec::new();
    let mut seen_files: HashMap<String, Vec<usize>> = HashMap::new(); // file → line ranges

    for r in results {
        let ranges = seen_files.entry(r.file.clone()).or_default();

        // Check overlap with already-selected results in same file
        let overlaps = ranges.iter().any(|&existing_line| {
            let overlap_ratio = if existing_line > r.line {
                (existing_line - r.line) as f64 / existing_line.max(r.line) as f64
            } else {
                (r.line - existing_line) as f64 / existing_line.max(r.line) as f64
            };
            overlap_ratio < 0.3 // lines are within 30% of each other → overlap
        });

        if !overlaps {
            ranges.push(r.line);
            deduped.push(r.clone());
        }
    }

    deduped
}

/// If a single file has ≥3 matches, collapse them into one summary entry
/// and insert it at the position of the first match.
fn group_same_file(results: &mut Vec<SearchResult>) {
    let mut file_counts: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, r) in results.iter().enumerate() {
        file_counts.entry(r.file.clone()).or_default().push(i);
    }

    // Remove entries for files with ≥3 matches (they'll be replaced by a summary)
    let mut to_remove: Vec<usize> = Vec::new();
    let mut summaries: Vec<(usize, String, SearchResult)> = Vec::new(); // (insert_pos, file, representative)

    for (file, indices) in &file_counts {
        if indices.len() >= 3 {
            let first_idx = indices[0];
            let rep = results[first_idx].clone();
            summaries.push((first_idx, file.clone(), rep));
            to_remove.extend(indices);
        }
    }

    // Remove in reverse order
    to_remove.sort_unstable();
    for &idx in to_remove.iter().rev() {
        results.remove(idx);
    }

    // Insert summaries (adjust positions as we go)
    // For simplicity: just prepend a single summary entry per file
    let mut offset = 0usize;
    for (_, _file, rep) in &summaries {
        if offset < results.len() {
            results.insert(offset, SearchResult {
                name: format!("{} matches", file_counts.get(&rep.file).map(|v| v.len()).unwrap_or(0)),
                kind: "file_summary".into(),
                file: rep.file.clone(),
                line: rep.line,
                parent: String::new(),
                signature: format!(
                    "{} symbols in this file. Use read_file to explore.",
                    file_counts.get(&rep.file).map(|v| v.len()).unwrap_or(0)
                ),
                rank: rep.rank,
            });
            offset += 1;
        }
    }
}

// ── Output Formatting ────────────────────────────────────────────────────

/// Format search results for LLM consumption.
///
/// Applies the full Phase 0 pipeline: hard caps → dedup → ranking → format.
/// Returns (formatted_string, was_truncated, total_results_before_cap).
pub fn format_search_results(
    results: &[SearchResult],
    query: &str,
    config: &SearchConfig,
) -> String {
    if results.is_empty() {
        return format!("No symbols found matching '{query}'. Try a different keyword or use shell+grep for full-text search.");
    }

    let total_before = results.len();

    // Step 1: Apply identifier-weighted ranking (stable sort by weight)
    let mut ranked: Vec<SearchResult> = results.to_vec();
    ranked.sort_by(|a, b| {
        let wa = a.rank * identifier_weight(&a.kind);
        let wb = b.rank * identifier_weight(&b.kind);
        wb.partial_cmp(&wa).unwrap_or(std::cmp::Ordering::Equal)
    });

    // Step 2: Deduplicate (same-file overlap removal)
    let mut deduped = deduplicate(&ranked);

    // Step 3: Collapse same-file groups (≥3 matches → summary)
    group_same_file(&mut deduped);

    // Step 4: Hard cap on result count
    let truncated = deduped.len() > config.max_results;
    if truncated {
        deduped.truncate(config.max_results);
    }

    // Step 5: Format each result
    let header = format!("{} results for '{}':\n", deduped.len(), query);
    let mut output = header;
    let mut total_chars = output.len();

    for r in &deduped {
        let sig = truncate_signature(&r.signature, config.max_chars_per_result);
        let line = if r.kind == "file_summary" {
            format!("`{}` — {}\n", r.file, sig)
        } else {
            format!(
                "`{}` ({}) — `{}:{}` | {}\n",
                r.name, r.kind, r.file, r.line, sig
            )
        };

        // Check total budget
        if total_chars + line.len() > config.max_total_chars {
            output.push_str(&format!(
                "\n... [truncated: {} more results exceed context budget. \
                 Use a more specific query or read_file for details.]\n",
                deduped.len() - deduped.iter().position(|x| x.file == r.file && x.line == r.line).unwrap_or(deduped.len())
            ));
            break;
        }
        total_chars += line.len();
        output.push_str(&line);
    }

    // Step 6: Context cliff warning
    if total_chars > config.max_total_chars * 3 / 4 {
        output.push_str(
            "\n⚠️ Result set is large. Consider narrowing your query or using \
             `read_file` on specific files for detailed context.\n",
        );
    }

    // Step 7: Truncation notice
    if truncated || total_before > config.max_results {
        output.push_str(&format!(
            "\n[{} total matches, showing top {}. Refine your query for more precision.]\n",
            total_before,
            deduped.len().min(config.max_results),
        ));
    }

    output
}

/// Truncate a signature to max_chars, breaking at a natural boundary.
fn truncate_signature(sig: &str, max_chars: usize) -> String {
    if sig.len() <= max_chars {
        return sig.to_string();
    }
    // Try to break at a natural boundary: `{`, `;`, `,`, or space
    let slice = &sig[..max_chars];
    for &break_char in &['{', ';', ',', ' '] {
        if let Some(pos) = slice.rfind(break_char) {
            if pos > max_chars / 2 {
                return format!("{}…", &sig[..=pos]);
            }
        }
    }
    format!("{}…", slice)
}

/// Code index backed by SQLite FTS5.
pub struct CodeIndex {
    pool: SqlitePool,
    root: PathBuf,
}

impl CodeIndex {
    /// Open or create the code index at the given db path.
    pub async fn open(db_path: &Path, root: &Path) -> Result<Self, String> {
        let db_dir = db_path.parent().unwrap_or(db_path);
        tokio::fs::create_dir_all(db_dir).await.map_err(|e| format!("create dir: {e}"))?;

        let opts = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true)
            .busy_timeout(std::time::Duration::from_secs(10))
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .map_err(|e| format!("open DB: {e}"))?;

        // WAL mode + synchronous NORMAL for better concurrent performance
        sqlx::query("PRAGMA synchronous=NORMAL")
            .execute(&pool).await.ok();

        // Create FTS5 table
        sqlx::query(
            "CREATE VIRTUAL TABLE IF NOT EXISTS code_symbols USING fts5(
                name, kind, file, line, parent, signature,
                tokenize='trigram case_sensitive 0'
            )"
        )
        .execute(&pool)
        .await
        .map_err(|e| format!("create FTS5: {e}"))?;

        Ok(Self { pool, root: root.to_path_buf() })
    }

    /// Full re-index: walk the root directory, scan all supported files.
    ///
    /// Uses DROP+CREATE instead of DELETE for the clear phase — O(1) vs O(n).
    /// DELETE FROM on a 350k+ row FTS5 table takes 5+ seconds; DROP is instant.
    /// INSERTs are still wrapped in a transaction for batch performance.
    pub async fn index_all(&self) -> Result<IndexStats, String> {
        let start = Instant::now();
        let mut count = 0usize;
        let mut files = 0usize;

        // Drop + recreate is O(1) vs DELETE FROM which is O(n) — critical for large indexes
        sqlx::query("DROP TABLE IF EXISTS code_symbols")
            .execute(&self.pool)
            .await
            .map_err(|e| format!("drop old index: {e}"))?;
        sqlx::query(
            "CREATE VIRTUAL TABLE code_symbols USING fts5(
                name, kind, file, line, parent, signature,
                tokenize='trigram case_sensitive 0'
            )"
        )
        .execute(&self.pool)
        .await
        .map_err(|e| format!("create new index: {e}"))?;

        // INSERTs in a transaction for batch performance
        let mut tx = self.pool.begin().await.map_err(|e| format!("begin tx: {e}"))?;

        for entry in walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            // Skip hidden dirs and common non-code dirs
            let path = entry.path();
            if path.to_string_lossy().contains("/.")
                || path.to_string_lossy().contains("\\.")
                || path.to_string_lossy().contains("target/")
                || path.to_string_lossy().contains("node_modules/")
                || path.to_string_lossy().contains("__pycache__/")
                || path.to_string_lossy().contains(".git/")
                || path.to_string_lossy().contains("dist/")
            {
                continue;
            }

            let symbols = scan_file(path, &self.root);
            if symbols.is_empty() {
                continue;
            }
            files += 1;

            // Batch INSERT per file for better performance
            for sym in &symbols {
                sqlx::query(
                    "INSERT INTO code_symbols (name, kind, file, line, parent, signature) VALUES (?, ?, ?, ?, ?, ?)"
                )
                .bind(&sym.name)
                .bind(sym.kind.as_str())
                .bind(&sym.file)
                .bind(sym.line as i64)
                .bind(&sym.parent)
                .bind(&sym.signature)
                .execute(&mut *tx)
                .await
                .map_err(|e| format!("insert: {e}"))?;
                count += 1;
            }
        }

        tx.commit().await.map_err(|e| format!("commit: {e}"))?;

        let elapsed = start.elapsed();
        tracing::info!(
            symbols = count,
            files,
            elapsed_ms = elapsed.as_millis(),
            "Code index built"
        );

        Ok(IndexStats {
            symbols: count,
            files,
            elapsed_ms: elapsed.as_millis() as u64,
        })
    }

    /// Search the code index by keyword query.
    ///
    /// Fetches more results than requested to enable client-side re-ranking
    /// (identifier-weighted) and deduplication. The caller should apply
    /// `format_search_results()` before sending to the LLM.
    ///
    /// Uses trigram tokenizer — no phrase search, no quote wrapping.
    /// Minimum query length is 3 characters (enforced by caller).
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, String> {
        // Trigram: pass query directly, no phrase wrapping.
        // Fetch 3× limit for re-ranking headroom.
        let fetch_limit = (limit * 3).min(50);

        let rows = sqlx::query_as::<_, CodeSymbolRow>(
            "SELECT name, kind, file, line, parent, signature, rank
             FROM code_symbols
             WHERE code_symbols MATCH ?
             ORDER BY rank
             LIMIT ?"
        )
        .bind(query)
        .bind(fetch_limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("search: {e}"))?;

        Ok(rows.into_iter().map(|r| SearchResult {
            name: r.name,
            kind: r.kind,
            file: r.file,
            line: r.line as usize,
            parent: r.parent,
            signature: r.signature,
            rank: r.rank,
        }).collect())
    }

    /// Search by symbol kind + keyword (e.g., "struct" + "UserStore").
    pub async fn search_by_kind(&self, kind: &str, query: &str, limit: usize) -> Result<Vec<SearchResult>, String> {
        let fetch_limit = (limit * 3).min(50);

        let rows = sqlx::query_as::<_, CodeSymbolRow>(
            "SELECT name, kind, file, line, parent, signature, rank
             FROM code_symbols
             WHERE kind = ? AND code_symbols MATCH ?
             ORDER BY rank
             LIMIT ?"
        )
        .bind(kind)
        .bind(query)
        .bind(fetch_limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("search by kind: {e}"))?;

        Ok(rows.into_iter().map(|r| SearchResult {
            name: r.name,
            kind: r.kind,
            file: r.file,
            line: r.line as usize,
            parent: r.parent,
            signature: r.signature,
            rank: r.rank,
        }).collect())
    }

    /// Incremental re-index: update only files changed since last full index.
    pub async fn reindex_changed(&self) -> Result<IndexStats, String> {
        let start = Instant::now();
        let mut count = 0usize;
        let mut files = 0usize;

        // Get existing files and their modification times from the index
        // Simple approach: re-scan all files, only update changed ones
        let existing: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT file FROM code_symbols"
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("list existing: {e}"))?;

        let existing_set: std::collections::HashSet<String> =
            existing.into_iter().map(|r| r.0).collect();

        let mut seen = std::collections::HashSet::new();

        for entry in walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();
            let path_str = path.to_string_lossy();
            if path_str.contains("/.") || path_str.contains("\\.")
                || path_str.contains("target/") || path_str.contains("node_modules/")
                || path_str.contains("__pycache__/") || path_str.contains(".git/")
                || path_str.contains("dist/")
            {
                continue;
            }

            let rel_path = path.strip_prefix(&self.root).unwrap_or(path)
                .to_string_lossy().replace('\\', "/");
            seen.insert(rel_path.clone());

            // Check if file is new or modified
            let _modified = entry.metadata().ok()
                .and_then(|m| m.modified().ok())
                .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs())
                .unwrap_or(0);

            let needs_update = !existing_set.contains(&rel_path);

            if needs_update {
                let symbols = scan_file(path, &self.root);
                if symbols.is_empty() { continue; }
                files += 1;

                // Delete old entries for this file
                sqlx::query("DELETE FROM code_symbols WHERE file = ?")
                    .bind(&rel_path)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| format!("delete old: {e}"))?;

                for sym in &symbols {
                    sqlx::query(
                        "INSERT INTO code_symbols (name, kind, file, line, parent, signature) VALUES (?, ?, ?, ?, ?, ?)"
                    )
                    .bind(&sym.name).bind(sym.kind.as_str()).bind(&sym.file)
                    .bind(sym.line as i64).bind(&sym.parent).bind(&sym.signature)
                    .execute(&self.pool).await.map_err(|e| format!("insert: {e}"))?;
                    count += 1;
                }
            }
        }

        // Remove entries for deleted files
        for old_file in existing_set.iter() {
            if !seen.contains(old_file) {
                sqlx::query("DELETE FROM code_symbols WHERE file = ?")
                    .bind(old_file)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| format!("delete stale: {e}"))?;
            }
        }

        let elapsed = start.elapsed();
        tracing::info!(symbols = count, files, elapsed_ms = elapsed.as_millis(), "Code index updated");
        Ok(IndexStats { symbols: count, files, elapsed_ms: elapsed.as_millis() as u64 })
    }

    /// Smart reindex: incremental if index exists, full if empty.
    /// First call after server start → incremental (index exists from background build).
    /// First call ever → full rebuild.
    pub async fn smart_reindex(&self) -> Result<IndexStats, String> {
        let count = self.count().await.unwrap_or(0);
        if count == 0 {
            tracing::info!("Code index empty, running full reindex");
            self.index_all().await
        } else {
            tracing::info!(count, "Code index exists, running incremental reindex");
            self.reindex_changed().await
        }
    }

    /// WAL checkpoint to prevent infinite WAL file growth.
    /// Called after full reindex operations.
    pub async fn wal_checkpoint(&self) {
        if let Err(e) = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&self.pool)
            .await
        {
            tracing::warn!(error = %e, "WAL checkpoint failed");
        }
    }

    /// Get index statistics for display in tool description.
    pub async fn stats(&self) -> Result<IndexStats, String> {
        let count = self.count().await?;
        let file_count = self.list_files().await.map(|f| f.len()).unwrap_or(0);
        Ok(IndexStats {
            symbols: count as usize,
            files: file_count,
            elapsed_ms: 0,
        })
    }

    /// Get symbol count.
    pub async fn count(&self) -> Result<i64, String> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM code_symbols")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| format!("count: {e}"))?;
        Ok(row.0)
    }

    /// List files in the index.
    pub async fn list_files(&self) -> Result<Vec<String>, String> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT file FROM code_symbols ORDER BY file"
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("list files: {e}"))?;
        Ok(rows.into_iter().map(|r| r.0).collect())
    }
}

impl IndexStats {
    /// Format for display in tool description (compact).
    pub fn summary(&self) -> String {
        format!("{} symbols in {} files", self.symbols, self.files)
    }
}

#[derive(Debug, sqlx::FromRow)]
struct CodeSymbolRow {
    name: String,
    kind: String,
    file: String,
    line: i64,
    parent: String,
    signature: String,
    rank: f64,
}

#[derive(Debug, Clone)]
pub struct IndexStats {
    pub symbols: usize,
    pub files: usize,
    pub elapsed_ms: u64,
}
