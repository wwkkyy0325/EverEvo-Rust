//! Background writer thread — non-blocking SQLite persistence for telemetry.

use std::path::Path;
use std::sync::mpsc;

use super::config::WriteCmd;

// ── Background writer ──────────────────────────────────────────────────────

/// Main writer loop. Runs in a dedicated thread, blocking on SQLite writes.
pub(crate) fn run_writer(rx: mpsc::Receiver<WriteCmd>, db_path: &Path) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(error = %e, "failed to build telemetry writer runtime");
            return;
        }
    };

    let pool = match rt.block_on(create_pool(db_path)) {
        Ok(pool) => pool,
        Err(e) => {
            tracing::error!(error = %e, "failed to open telemetry database");
            return;
        }
    };

    if let Err(e) = rt.block_on(create_tables(&pool)) {
        tracing::error!(error = %e, "failed to create telemetry tables");
        return;
    }

    for cmd in rx {
        rt.block_on(execute_cmd(&pool, cmd));
    }

    // Close the pool before the runtime shuts down.
    rt.block_on(pool.close());
}

pub(crate) async fn create_pool(db_path: &Path) -> Result<sqlx::SqlitePool, sqlx::Error> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true);
    sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
}

async fn create_tables(pool: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(CREATE_SPANS_TABLE).execute(pool).await?;
    sqlx::query(CREATE_RETRIEVALS_TABLE).execute(pool).await?;
    sqlx::query(CREATE_AGENT_TURNS_TABLE).execute(pool).await?;
    Ok(())
}

async fn execute_cmd(pool: &sqlx::SqlitePool, cmd: WriteCmd) {
    let result = match cmd {
        WriteCmd::Span {
            id,
            trace_id,
            parent_id,
            name,
            started_at,
            duration_ms,
            status,
            metadata,
            metrics,
        } => {
            sqlx::query(
                "INSERT INTO telemetry_spans \
                 (id, trace_id, parent_id, name, started_at, duration_ms, status, metadata, metrics) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&trace_id)
            .bind(&parent_id)
            .bind(&name)
            .bind(&started_at)
            .bind(duration_ms)
            .bind(&status)
            .bind(&metadata)
            .bind(&metrics)
            .execute(pool)
            .await
            .map(|_| ())
        }
        WriteCmd::Retrieval {
            id,
            trace_id,
            query,
            source,
            recall_k,
            precision_at_5,
            mrr,
            latency_ms,
            experiment_id,
            variant,
        } => {
            sqlx::query(
                "INSERT INTO telemetry_retrievals \
                 (id, trace_id, query, source, recall_k, precision_at_5, mrr, latency_ms, experiment_id, variant) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&trace_id)
            .bind(&query)
            .bind(&source)
            .bind(recall_k)
            .bind(precision_at_5)
            .bind(mrr)
            .bind(latency_ms)
            .bind(&experiment_id)
            .bind(&variant)
            .execute(pool)
            .await
            .map(|_| ())
        }
        WriteCmd::AgentTurn {
            id,
            trace_id,
            turn_number,
            tool_calls_total,
            tool_calls_success,
            task_completed,
            latency_ms,
            tokens_input,
            tokens_output,
            error_type,
            error_message,
            experiment_id,
            variant,
        } => {
            sqlx::query(
                "INSERT INTO telemetry_agent_turns \
                 (id, trace_id, turn_number, tool_calls_total, tool_calls_success, task_completed, latency_ms, tokens_input, tokens_output, error_type, error_message, experiment_id, variant) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&trace_id)
            .bind(turn_number)
            .bind(tool_calls_total)
            .bind(tool_calls_success)
            .bind(task_completed)
            .bind(latency_ms)
            .bind(tokens_input)
            .bind(tokens_output)
            .bind(&error_type)
            .bind(&error_message)
            .bind(&experiment_id)
            .bind(&variant)
            .execute(pool)
            .await
            .map(|_| ())
        }
        WriteCmd::Flush(tx) => {
            tx.send(()).ok();
            return;
        }
    };

    if let Err(e) = result {
        tracing::error!(error = %e, "telemetry write failed");
    }
}

// ── SQL DDL ────────────────────────────────────────────────────────────────

const CREATE_SPANS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS telemetry_spans (
    id TEXT PRIMARY KEY,
    trace_id TEXT NOT NULL,
    parent_id TEXT,
    name TEXT NOT NULL,
    started_at TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'ok',
    metadata TEXT NOT NULL DEFAULT '{}',
    metrics TEXT NOT NULL DEFAULT '{}'
);
"#;

const CREATE_RETRIEVALS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS telemetry_retrievals (
    id TEXT PRIMARY KEY,
    trace_id TEXT NOT NULL,
    query TEXT NOT NULL,
    source TEXT NOT NULL,
    recall_k INTEGER NOT NULL,
    precision_at_5 REAL,
    mrr REAL,
    latency_ms INTEGER NOT NULL,
    experiment_id TEXT,
    variant TEXT
);
"#;

const CREATE_AGENT_TURNS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS telemetry_agent_turns (
    id TEXT PRIMARY KEY,
    trace_id TEXT NOT NULL,
    turn_number INTEGER NOT NULL,
    tool_calls_total INTEGER NOT NULL DEFAULT 0,
    tool_calls_success INTEGER NOT NULL DEFAULT 0,
    task_completed INTEGER NOT NULL DEFAULT 0,
    latency_ms INTEGER NOT NULL,
    tokens_input INTEGER NOT NULL DEFAULT 0,
    tokens_output INTEGER NOT NULL DEFAULT 0,
    error_type TEXT,
    error_message TEXT,
    experiment_id TEXT,
    variant TEXT
);
"#;
