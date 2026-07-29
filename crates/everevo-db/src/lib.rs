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
        };

        let pool = SqlitePool::connect_with(options)
            .await
            .map_err(|e| EverEvoError::Database(format!("Failed to connect: {e}")))?;

        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .map_err(|e| EverEvoError::Database(format!("Migration failed: {e}")))?;

        Ok(Self { pool })
    }
}
