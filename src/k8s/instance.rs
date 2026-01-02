use std::time::Instant;

/// State of a PR instance in its lifecycle
#[derive(Debug, Clone)]
pub enum InstanceState {
    /// Jenkins build triggered, waiting for completion
    Building {
        started_at: Instant,
        build_url: Option<String>,
    },
    /// Build complete, creating k8s deployment
    Deploying { image_tag: String },
    /// Deployment created, waiting for pod to be ready
    Starting {
        deployment_name: String,
        address: String,
        port: u16,
        started_at: Instant,
    },
    /// Pod ready, server accepting connections
    Ready {
        deployment_name: String,
        address: String,
        port: u16,
        ready_at: Instant,
        shutdown_at: Instant,
    },
    /// Shutdown timer expired, deleting deployment
    ShuttingDown { deployment_name: String },
    /// Build or deployment failed
    Failed { reason: String },
}

impl InstanceState {
    /// Get a human-readable status text
    pub fn status_text(&self) -> &'static str {
        match self {
            InstanceState::Building { .. } => "building",
            InstanceState::Deploying { .. } => "deploying",
            InstanceState::Starting { .. } => "starting",
            InstanceState::Ready { .. } => "ready",
            InstanceState::ShuttingDown { .. } => "shutting down",
            InstanceState::Failed { .. } => "failed",
        }
    }
}

/// A PR instance being managed by the proxy
#[derive(Debug, Clone)]
pub struct PrInstance {
    pub pr_number: u32,
    pub commit_hash: String,
    pub commit_hash_short: String,
    pub state: InstanceState,
}

impl PrInstance {
    /// Create a new instance in Building state with commit info
    pub fn new_building(pr_number: u32, commit_hash: String, commit_hash_short: String, queue_url: String) -> Self {
        Self {
            pr_number,
            commit_hash,
            commit_hash_short,
            state: InstanceState::Building {
                started_at: Instant::now(),
                build_url: Some(queue_url),
            },
        }
    }

    /// Create a new instance in Deploying state (skipping build because image exists)
    pub fn new_deploying(pr_number: u32, commit_hash: String, commit_hash_short: String, image_tag: String) -> Self {
        Self {
            pr_number,
            commit_hash,
            commit_hash_short,
            state: InstanceState::Deploying { image_tag },
        }
    }

    /// Get the image tag for this instance
    pub fn image_tag(&self, registry: &str, image_name: &str) -> String {
        format!("{}/{}:pr-{}-{}", registry, image_name, self.pr_number, self.commit_hash_short)
    }
}
