//! EverEvo database layer.
//!
//! Provides SQLx-backed CRUD for sessions, messages, and document metadata.
//! Uses SQLite by default; PostgreSQL supported via feature flag (TODO).

pub mod models;
pub mod queries;

use std::path::Path;

use everevo_core::EverEvoError;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};

/// Thin wrapper around the connection pool for dependency injection.
#[derive(Clone)]
pub struct Database {
    pub pool: SqlitePool,
}

impl Database {
    /// Create a new Database from an existing pool.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Open a SQLite database at the given path and run migrations.
    ///
    /// Uses `SqliteConnectOptions` directly to avoid URL-encoding issues
    /// with Windows paths. Pass `":memory:"` for in-memory databases.
    pub async fn connect(path: &Path) -> Result<Self, EverEvoError> {
        let options = if path == Path::new(":memory:") {
            use std::str::FromStr;
            SqliteConnectOptions::from_str("sqlite::memory:?cache=shared")
                .map_err(|e| EverEvoError::Database(format!("Invalid in-memory URL: {e}")))?
        } else {
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true)
                .foreign_keys(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .busy_timeout(std::time::Duration::from_secs(30))
        };

        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(6)
            .acquire_timeout(std::time::Duration::from_secs(15))
            .connect_with(options)
            .await
            .map_err(|e| EverEvoError::Database(format!("Failed to connect: {e}")))?;

        sqlx::migrate!("../../../migrations")
            .run(&pool)
            .await
            .map_err(|e| EverEvoError::Database(format!("Migration failed: {e}")))?;

        // Tune WAL for multi-writer safety under burst writes (fact upsert +
        // message save + telemetry inserts in the same agent turn).
        //
        // synchronous=NORMAL is safe in WAL mode and ~20× faster for multi-
        // statement transactions than FULL (default). The WAL itself protects
        // against corruption on power loss.
        sqlx::query("PRAGMA synchronous=NORMAL")
            .execute(&pool)
            .await
            .ok();
        // Raise WAL autocheckpoint threshold to reduce snapshot-invalidation
        // frequency (BUSY_SNAPSHOT 517). Default is 1000 pages (~4 MB); 10000
        // pages (~40 MB) means checkpoints happen 10× less often, giving
        // concurrent writers much more time to finish before the WAL is pruned.
        sqlx::query("PRAGMA wal_autocheckpoint=10000")
            .execute(&pool)
            .await
            .ok();

        Ok(Self { pool })
    }
}
