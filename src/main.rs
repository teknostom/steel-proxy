use anyhow::Result;
use log::{error, info, warn};
use std::sync::Arc;
use tokio::net::TcpListener;

mod config;
mod db;
mod github;
mod jenkins;
mod k8s;
mod packets;
mod proxy;

use config::ProxyConfig;
use github::GitHubClient;
use jenkins::JenkinsClient;
use k8s::K8sManager;
use proxy::ProxyServer;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logger
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    info!("Starting Steel Proxy Server...");

    // Load configuration
    let config = ProxyConfig::load("proxy-config.toml")?;
    info!(
        "Loaded configuration: {} backend servers",
        config.backends.len()
    );

    // Initialize database
    let db_pool = db::init(&config.database_path).await?;
    info!("Database initialized: {}", config.database_path);

    // Create GitHub client if configured
    let github_client = config.github.as_ref().map(|cfg| {
        info!("GitHub integration enabled: {}/{}", cfg.owner, cfg.repo);
        Arc::new(GitHubClient::new(cfg.clone()))
    });

    // Create Jenkins client if configured
    let jenkins_client = config.jenkins.as_ref().map(|cfg| {
        info!("Jenkins integration enabled: {}", cfg.url);
        Arc::new(JenkinsClient::new(cfg.clone()))
    });

    // Create K8s manager if configured
    let k8s_manager = config.kubernetes.as_ref().map(|cfg| {
        info!("Kubernetes integration enabled: namespace={}", cfg.namespace);
        Arc::new(K8sManager::new(cfg.clone(), db_pool.clone()))
    });

    // Create proxy server
    let proxy = Arc::new(ProxyServer::new(
        config.clone(),
        k8s_manager.clone(),
        jenkins_client.clone(),
        github_client,
        db_pool,
    ));

    // Spawn lifecycle loop if k8s and jenkins are configured
    if let (Some(k8s_manager), Some(jenkins_client)) = (k8s_manager, jenkins_client) {
        // Recover any running instances from database
        match k8s_manager.recover_from_db(&proxy).await {
            Ok(0) => info!("No instances to recover from database"),
            Ok(n) => info!("Recovered {} instances from database", n),
            Err(e) => warn!("Failed to recover instances from database: {}", e),
        }

        let proxy_for_lifecycle = proxy.clone();
        tokio::spawn(async move {
            info!("Starting PR instance lifecycle manager");
            k8s::run_lifecycle_loop(proxy_for_lifecycle, k8s_manager, jenkins_client).await;
        });
    }

    // Bind to proxy port
    let listener = TcpListener::bind(format!("{}:{}", config.bind_address, config.bind_port))
        .await?;
    info!(
        "Proxy listening on {}:{}",
        config.bind_address, config.bind_port
    );

    // Accept connections
    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                info!("New connection from {}", addr);
                let proxy = proxy.clone();
                tokio::spawn(async move {
                    if let Err(e) = proxy.handle_client(stream, addr).await {
                        error!("Error handling client {}: {}", addr, e);
                    }
                });
            }
            Err(e) => {
                error!("Error accepting connection: {}", e);
            }
        }
    }
}
