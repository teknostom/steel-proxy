use anyhow::Result;
use sqlx::sqlite::SqlitePool;
use sqlx::FromRow;

/// Represents a PR build in the database
#[derive(Debug, Clone, FromRow)]
pub struct PrBuild {
    pub id: i64,
    pub pr_number: i64,
    pub commit_hash: String,
    pub commit_hash_short: String,
    pub image_tag: String,
    pub state: String,
    pub state_data: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl PrBuild {
    /// Get a PR build by PR number
    pub async fn get_by_pr(pool: &SqlitePool, pr_number: u32) -> Result<Option<PrBuild>> {
        let build = sqlx::query_as::<_, PrBuild>(
            "SELECT * FROM pr_builds WHERE pr_number = ?",
        )
        .bind(pr_number as i64)
        .fetch_optional(pool)
        .await?;

        Ok(build)
    }

    /// Check if a build exists for this PR with the given commit hash
    pub async fn exists_with_commit(
        pool: &SqlitePool,
        pr_number: u32,
        commit_hash_short: &str,
    ) -> Result<bool> {
        let result = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM pr_builds WHERE pr_number = ? AND commit_hash_short = ? AND state = 'Ready'",
        )
        .bind(pr_number as i64)
        .bind(commit_hash_short)
        .fetch_one(pool)
        .await?;

        Ok(result > 0)
    }

    /// Create or update a PR build
    pub async fn upsert(
        pool: &SqlitePool,
        pr_number: u32,
        commit_hash: &str,
        commit_hash_short: &str,
        image_tag: &str,
        state: &str,
        state_data: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO pr_builds (pr_number, commit_hash, commit_hash_short, image_tag, state, state_data, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, datetime('now'))
            ON CONFLICT(pr_number) DO UPDATE SET
                commit_hash = excluded.commit_hash,
                commit_hash_short = excluded.commit_hash_short,
                image_tag = excluded.image_tag,
                state = excluded.state,
                state_data = excluded.state_data,
                updated_at = datetime('now')
            "#,
        )
        .bind(pr_number as i64)
        .bind(commit_hash)
        .bind(commit_hash_short)
        .bind(image_tag)
        .bind(state)
        .bind(state_data)
        .execute(pool)
        .await?;

        Ok(())
    }

    /// Update just the state and state_data for a PR build
    pub async fn update_state(
        pool: &SqlitePool,
        pr_number: u32,
        state: &str,
        state_data: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE pr_builds
            SET state = ?, state_data = ?, updated_at = datetime('now')
            WHERE pr_number = ?
            "#,
        )
        .bind(state)
        .bind(state_data)
        .bind(pr_number as i64)
        .execute(pool)
        .await?;

        Ok(())
    }

    /// Delete a PR build record
    pub async fn delete(pool: &SqlitePool, pr_number: u32) -> Result<()> {
        sqlx::query("DELETE FROM pr_builds WHERE pr_number = ?")
            .bind(pr_number as i64)
            .execute(pool)
            .await?;

        Ok(())
    }

    /// Get all active builds (not Failed or ShuttingDown)
    pub async fn get_active_builds(pool: &SqlitePool) -> Result<Vec<PrBuild>> {
        let builds = sqlx::query_as::<_, PrBuild>(
            "SELECT * FROM pr_builds WHERE state NOT IN ('Failed', 'ShuttingDown')",
        )
        .fetch_all(pool)
        .await?;

        Ok(builds)
    }

    /// Get the last successful build for a PR (to check if we can skip rebuild)
    pub async fn get_last_ready_commit(pool: &SqlitePool, pr_number: u32) -> Result<Option<String>> {
        let result = sqlx::query_scalar::<_, String>(
            "SELECT commit_hash_short FROM pr_builds WHERE pr_number = ? AND state = 'Ready' ORDER BY updated_at DESC LIMIT 1",
        )
        .bind(pr_number as i64)
        .fetch_optional(pool)
        .await?;

        Ok(result)
    }
}
