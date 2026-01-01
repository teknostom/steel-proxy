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
    pub state: InstanceState,
}

impl PrInstance {
    /// Create a new instance in Building state
    pub fn new(pr_number: u32) -> Self {
        Self {
            pr_number,
            state: InstanceState::Building {
                started_at: Instant::now(),
                build_url: None,
            },
        }
    }

    /// Create a new instance in Building state with a queue URL
    pub fn new_with_queue_url(pr_number: u32, queue_url: String) -> Self {
        Self {
            pr_number,
            state: InstanceState::Building {
                started_at: Instant::now(),
                build_url: Some(queue_url),
            },
        }
    }
}
