use anyhow::Result;
use log::{error, info};
use sqlx::sqlite::SqlitePool;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio::sync::RwLock;

mod backend_connector;
mod client_handler;
mod packet_forwarder;

use client_handler::ClientHandler;

use crate::config::{BackendServer, ProxyConfig};
use crate::github::GitHubClient;
use crate::jenkins::JenkinsClient;
use crate::k8s::K8sManager;

/// Registry of backend servers, supporting both static (from config) and dynamic (from k8s) backends
pub struct BackendRegistry {
    /// Static backends from configuration file (e.g., master, dev)
    static_backends: HashMap<String, BackendServer>,
    /// Dynamic backends from k8s (e.g., PR instances)
    dynamic_backends: HashMap<String, BackendServer>,
}

impl BackendRegistry {
    pub fn new(static_backends: HashMap<String, BackendServer>) -> Self {
        Self {
            static_backends,
            dynamic_backends: HashMap::new(),
        }
    }

    /// Get a backend by name, checking dynamic backends first
    pub fn get(&self, name: &str) -> Option<&BackendServer> {
        self.dynamic_backends
            .get(name)
            .or_else(|| self.static_backends.get(name))
    }

    /// Check if a backend exists
    pub fn contains(&self, name: &str) -> bool {
        self.dynamic_backends.contains_key(name) || self.static_backends.contains_key(name)
    }

    /// List all backends (static and dynamic)
    pub fn iter(&self) -> impl Iterator<Item = (&String, &BackendServer)> {
        self.static_backends.iter().chain(self.dynamic_backends.iter())
    }

    /// Add a dynamic backend
    pub fn add_dynamic(&mut self, name: String, backend: BackendServer) {
        self.dynamic_backends.insert(name, backend);
    }

    /// Remove a dynamic backend
    pub fn remove_dynamic(&mut self, name: &str) -> Option<BackendServer> {
        self.dynamic_backends.remove(name)
    }
}

/// Receiver for broadcast messages (held by each PacketForwarder)
pub type BroadcastReceiver = broadcast::Receiver<String>;

pub struct ProxyServer {
    config: ProxyConfig,
    backends: Arc<RwLock<BackendRegistry>>,
    next_client_id: parking_lot::Mutex<u64>,
    /// Stores pending server switches: UUID -> target server name
    pending_switches: parking_lot::Mutex<HashMap<uuid::Uuid, String>>,
    /// Broadcast channel for sending messages to all connected players
    broadcast_tx: broadcast::Sender<String>,
    /// K8s manager for PR instances (None if k8s not configured)
    k8s_manager: Option<Arc<K8sManager>>,
    /// Jenkins client for triggering builds (None if jenkins not configured)
    jenkins_client: Option<Arc<JenkinsClient>>,
    /// GitHub client for fetching PR info (None if github not configured)
    github_client: Option<Arc<GitHubClient>>,
    /// Database connection pool
    db_pool: SqlitePool,
}

impl ProxyServer {
    pub fn new(
        config: ProxyConfig,
        k8s_manager: Option<Arc<K8sManager>>,
        jenkins_client: Option<Arc<JenkinsClient>>,
        github_client: Option<Arc<GitHubClient>>,
        db_pool: SqlitePool,
    ) -> Self {
        let backends = BackendRegistry::new(config.backends.clone());
        let (broadcast_tx, _) = broadcast::channel(16);

        Self {
            config,
            backends: Arc::new(RwLock::new(backends)),
            next_client_id: parking_lot::Mutex::new(0),
            pending_switches: parking_lot::Mutex::new(HashMap::new()),
            broadcast_tx,
            k8s_manager,
            jenkins_client,
            github_client,
            db_pool,
        }
    }

    /// Get a reference to the database pool
    pub fn db_pool(&self) -> &SqlitePool {
        &self.db_pool
    }

    /// Get a reference to the GitHub client
    pub fn github_client(&self) -> Option<&Arc<GitHubClient>> {
        self.github_client.as_ref()
    }

    /// Get a reference to the K8s manager
    pub fn k8s_manager(&self) -> Option<&Arc<K8sManager>> {
        self.k8s_manager.as_ref()
    }

    /// Get the K8s manager Arc clone (for spawning lifecycle loop)
    pub fn k8s_manager_arc(&self) -> Option<Arc<K8sManager>> {
        self.k8s_manager.clone()
    }

    /// Get a reference to the Jenkins client
    pub fn jenkins_client(&self) -> Option<&Arc<JenkinsClient>> {
        self.jenkins_client.as_ref()
    }

    /// Get the Jenkins client Arc clone (for spawning lifecycle loop)
    pub fn jenkins_client_arc(&self) -> Option<Arc<JenkinsClient>> {
        self.jenkins_client.clone()
    }

    /// Get a reference to the shared backend registry
    pub fn backends(&self) -> Arc<RwLock<BackendRegistry>> {
        self.backends.clone()
    }

    /// Get a reference to the config
    pub fn config(&self) -> &ProxyConfig {
        &self.config
    }

    pub fn get_target_server(&self, uuid: uuid::Uuid) -> Option<String> {
        self.pending_switches.lock().remove(&uuid)
    }

    pub fn set_target_server(&self, uuid: uuid::Uuid, server: String) {
        self.pending_switches.lock().insert(uuid, server);
    }

    /// Subscribe to broadcast messages (called by each PacketForwarder)
    pub fn subscribe_broadcast(&self) -> BroadcastReceiver {
        self.broadcast_tx.subscribe()
    }

    /// Broadcast a message to all connected players
    pub fn broadcast(&self, message: &str) {
        // Ignore error if no receivers
        let _ = self.broadcast_tx.send(message.to_string());
    }

    /// Start a PR instance build
    /// Returns Ok(()) if build was triggered, Err if something went wrong
    pub async fn start_pr(&self, pr_number: u32) -> Result<()> {
        // Check if GitHub is configured (needed for commit lookup)
        let github = self.github_client.as_ref()
            .ok_or_else(|| anyhow::anyhow!("GitHub not configured"))?;

        // Check if Jenkins is configured
        let jenkins = self.jenkins_client.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Jenkins not configured"))?;

        // Check if K8s is configured
        let k8s = self.k8s_manager.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Kubernetes not configured"))?;

        // Check if this PR is already being processed
        if k8s.has_instance(pr_number) {
            anyhow::bail!("PR #{} is already being built or running", pr_number);
        }

        // Check if backend already exists (dynamic PR instance)
        let backend_name = format!("pr-{}", pr_number);
        let backends = self.backends.read().await;
        if backends.contains(&backend_name) {
            anyhow::bail!("PR #{} instance already exists", pr_number);
        }
        drop(backends);

        // Fetch current commit hash from GitHub
        self.broadcast(&format!("§7[Proxy] Fetching PR #{} info from GitHub...", pr_number));
        let (commit_hash, commit_hash_short) = github.get_pr_head_commit(pr_number).await?;
        info!("PR #{} head commit: {}", pr_number, commit_hash_short);

        // Check if we already have an image with this commit (can skip build)
        let existing_build = crate::db::PrBuild::exists_with_commit(
            &self.db_pool,
            pr_number,
            &commit_hash_short,
        ).await?;

        if existing_build {
            // Image already exists, skip directly to deploy
            info!("PR #{} image already exists for commit {}, skipping build", pr_number, commit_hash_short);
            self.broadcast(&format!(
                "§a[Proxy] PR #{} already built for commit {}. Deploying...",
                pr_number, commit_hash_short
            ));

            k8s.create_deploying_instance(pr_number, commit_hash, commit_hash_short);
        } else {
            // Need to build new image
            info!("Starting build for PR #{} commit {}", pr_number, commit_hash_short);
            self.broadcast(&format!("§e[Proxy] Building PR #{} (commit {})...", pr_number, commit_hash_short));

            let queue_url = jenkins.trigger_build(pr_number, &commit_hash_short).await?;

            // Create instance in Building state
            k8s.create_building_instance(pr_number, commit_hash, commit_hash_short, queue_url);

            self.broadcast(&format!("§a[Proxy] PR #{} build queued. You will be notified when it's ready.", pr_number));
        }

        Ok(())
    }

    pub async fn handle_client(self: Arc<Self>, stream: TcpStream, addr: SocketAddr) -> Result<()> {
        let client_id = {
            let mut id = self.next_client_id.lock();
            *id += 1;
            *id
        };

        info!("[Client {}] Connected from {}", client_id, addr);

        // Create client handler
        let handler = ClientHandler::new(client_id, stream, addr, self.clone());

        // Handle client connection
        if let Err(e) = handler.run().await {
            error!("[Client {}] Error: {}", client_id, e);
        }

        info!("[Client {}] Disconnected", client_id);
        Ok(())
    }
}
