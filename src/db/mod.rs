use anyhow::Result;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::path::Path;

mod models;
pub use models::PrBuild;

/// Initialize the database connection pool and run migrations
pub async fn init(database_path: &str) -> Result<SqlitePool> {
    // Create parent directories if they don't exist
    if let Some(parent) = Path::new(database_path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    // SQLite connection string with create_if_missing
    let database_url = format!("sqlite:{}?mode=rwc", database_path);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;

    // Run migrations
    run_migrations(&pool).await?;

    Ok(pool)
}

async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS pr_builds (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            pr_number INTEGER NOT NULL UNIQUE,
            commit_hash TEXT NOT NULL,
            commit_hash_short TEXT NOT NULL,
            image_tag TEXT NOT NULL,
            state TEXT NOT NULL,
            state_data TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )
        "#,
    )
    .execute(pool)
    .await?;

    // Create index for faster lookups
    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_pr_builds_pr_number ON pr_builds(pr_number)
        "#,
    )
    .execute(pool)
    .await?;

    Ok(())
}
