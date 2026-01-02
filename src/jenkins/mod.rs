use anyhow::{Context, Result};
use log::info;
use reqwest::header::HeaderValue;

use crate::config::JenkinsConfig;

/// Jenkins build status
#[derive(Debug, Clone, PartialEq)]
pub enum BuildStatus {
    /// Build is queued, waiting to start
    Queued,
    /// Build is running
    Building,
    /// Build completed successfully
    Success,
    /// Build failed
    Failed { reason: String },
    /// Build not found
    NotFound,
}

/// Client for interacting with Jenkins API
pub struct JenkinsClient {
    config: JenkinsConfig,
    http: reqwest::Client,
}

impl JenkinsClient {
    pub fn new(config: JenkinsConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    /// Trigger a build for a PR
    /// Returns the queue URL to poll for build status
    pub async fn trigger_build(&self, pr_number: u32, commit_hash_short: &str) -> Result<String> {
        let url = format!(
            "{}/job/{}/buildWithParameters",
            self.config.url, self.config.job_name
        );

        info!("Triggering Jenkins build for PR #{} commit {}", pr_number, commit_hash_short);

        let response: reqwest::Response = self
            .http
            .post(&url)
            .basic_auth(&self.config.username, Some(&self.config.api_token))
            .form(&[
                ("PR_NUMBER", pr_number.to_string()),
                ("COMMIT_HASH", commit_hash_short.to_string()),
            ])
            .send()
            .await
            .context("Failed to trigger Jenkins build")?;

        if !response.status().is_success() {
            let status = response.status();
            let text: String = response.text().await.unwrap_or_default();
            anyhow::bail!("Jenkins returned error: {} {}", status, text);
        }

        // Get queue location from header
        let queue_url = response
            .headers()
            .get("Location")
            .and_then(|h: &HeaderValue| h.to_str().ok())
            .map(|s: &str| s.to_string())
            .unwrap_or_else(|| format!("{}/queue/", self.config.url));

        info!("Build queued for PR #{} commit {}: {}", pr_number, commit_hash_short, queue_url);
        Ok(queue_url)
    }

    /// Get the build URL from a queue item
    pub async fn get_build_url(&self, queue_url: &str) -> Result<Option<String>> {
        let api_url = format!("{}api/json", queue_url);

        let response: reqwest::Response = self
            .http
            .get(&api_url)
            .basic_auth(&self.config.username, Some(&self.config.api_token))
            .send()
            .await
            .context("Failed to query queue item")?;

        if !response.status().is_success() {
            return Ok(None);
        }

        let json: serde_json::Value = response.json().await?;

        // Check if build has started (executable field is populated)
        if let Some(executable) = json.get("executable") {
            if let Some(url) = executable.get("url").and_then(|u| u.as_str()) {
                return Ok(Some(url.to_string()));
            }
        }

        Ok(None)
    }

    /// Get the status of a build
    pub async fn get_build_status(&self, build_url: &str) -> Result<BuildStatus> {
        let api_url = format!("{}api/json", build_url);

        let response: reqwest::Response = self
            .http
            .get(&api_url)
            .basic_auth(&self.config.username, Some(&self.config.api_token))
            .send()
            .await
            .context("Failed to query build status")?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(BuildStatus::NotFound);
        }

        if !response.status().is_success() {
            anyhow::bail!("Jenkins returned error: {}", response.status());
        }

        let json: serde_json::Value = response.json().await?;

        // Check if build is still running
        let building = json.get("building").and_then(|b| b.as_bool()).unwrap_or(false);
        if building {
            return Ok(BuildStatus::Building);
        }

        // Check result
        let result = json.get("result").and_then(|r| r.as_str()).unwrap_or("");
        match result {
            "SUCCESS" => Ok(BuildStatus::Success),
            "FAILURE" | "ABORTED" | "UNSTABLE" => Ok(BuildStatus::Failed {
                reason: result.to_string(),
            }),
            "" => Ok(BuildStatus::Queued), // No result yet, probably queued
            _ => Ok(BuildStatus::Failed {
                reason: format!("Unknown result: {}", result),
            }),
        }
    }

    /// Get the build timeout duration
    pub fn build_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.build_timeout_minutes * 60)
    }
}
