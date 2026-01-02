use anyhow::{Context, Result};
use log::debug;
use serde::Deserialize;

use crate::config::GitHubConfig;

/// Response from GitHub PR API
#[derive(Debug, Deserialize)]
struct PullRequest {
    head: PullRequestHead,
}

#[derive(Debug, Deserialize)]
struct PullRequestHead {
    sha: String,
}

/// GitHub API client for fetching PR information
pub struct GitHubClient {
    config: GitHubConfig,
    http: reqwest::Client,
}

impl GitHubClient {
    pub fn new(config: GitHubConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    /// Get the head commit SHA for a pull request
    /// Returns (full_sha, short_sha)
    pub async fn get_pr_head_commit(&self, pr_number: u32) -> Result<(String, String)> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}",
            self.config.owner, self.config.repo, pr_number
        );

        debug!("Fetching PR #{} from {}", pr_number, url);

        let mut request = self.http
            .get(&url)
            .header("User-Agent", "steel-proxy")
            .header("Accept", "application/vnd.github+json");

        // Add auth token if configured
        if let Some(ref token) = self.config.token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request
            .send()
            .await
            .context("Failed to fetch PR from GitHub")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("GitHub API error {}: {}", status, body);
        }

        let pr: PullRequest = response
            .json()
            .await
            .context("Failed to parse GitHub PR response")?;

        let full_sha = pr.head.sha;
        let short_sha = full_sha.chars().take(7).collect();

        debug!("PR #{} head commit: {} ({})", pr_number, short_sha, full_sha);

        Ok((full_sha, short_sha))
    }
}
