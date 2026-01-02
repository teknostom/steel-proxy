use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Address the proxy binds to
    #[serde(default = "default_bind_address")]
    pub bind_address: String,

    /// Port the proxy listens on
    #[serde(default = "default_bind_port")]
    pub bind_port: u16,

    /// Public address that clients should connect to (sent in Transfer packets)
    /// For local testing, use "localhost" or "127.0.0.1"
    /// For public servers, use the server's public IP or domain name
    #[serde(default = "default_public_address")]
    pub public_address: String,

    /// Enable online mode (Mojang authentication)
    #[serde(default = "default_online_mode")]
    pub online_mode: bool,

    /// Path to SQLite database file
    #[serde(default = "default_database_path")]
    pub database_path: String,

    /// Default server for new connections
    pub default_server: String,

    /// Backend servers
    pub backends: HashMap<String, BackendServer>,

    /// Kubernetes configuration (optional - enables PR instance management)
    pub kubernetes: Option<KubernetesConfig>,

    /// Jenkins configuration (optional - enables PR builds)
    pub jenkins: Option<JenkinsConfig>,

    /// GitHub configuration (optional - enables PR commit lookup)
    pub github: Option<GitHubConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KubernetesConfig {
    /// Kubernetes namespace for PR instances
    pub namespace: String,

    /// Docker registry for PR images (e.g., "localhost:5000/steel")
    pub registry: String,

    /// Instance auto-shutdown timeout in minutes (default: 30)
    #[serde(default = "default_instance_timeout")]
    pub instance_timeout_minutes: u64,

    /// Base image name (tag will be pr-<number>)
    #[serde(default = "default_image_name")]
    pub image_name: String,

    /// Resource limits for PR instances
    #[serde(default)]
    pub resources: ResourceLimits,

    /// Node address for connecting to NodePort services (e.g., "127.0.0.1")
    #[serde(default = "default_node_address")]
    pub node_address: String,
}

fn default_node_address() -> String {
    "127.0.0.1".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    /// Memory limit (e.g., "2Gi")
    #[serde(default = "default_memory_limit")]
    pub memory: String,

    /// CPU limit (e.g., "1000m")
    #[serde(default = "default_cpu_limit")]
    pub cpu: String,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            memory: default_memory_limit(),
            cpu: default_cpu_limit(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JenkinsConfig {
    /// Jenkins base URL (e.g., "https://jenkins.example.com")
    pub url: String,

    /// Job name for PR builds
    pub job_name: String,

    /// Username for authentication
    pub username: String,

    /// API token for authentication
    pub api_token: String,

    /// Build timeout in minutes (default: 15)
    #[serde(default = "default_build_timeout")]
    pub build_timeout_minutes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubConfig {
    /// Repository owner (e.g., "octocat")
    pub owner: String,

    /// Repository name (e.g., "hello-world")
    pub repo: String,

    /// Personal access token (optional - for private repos or higher rate limits)
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendServer {
    /// Server address
    pub address: String,

    /// Server port
    pub port: u16,

    /// Server description (optional)
    pub description: Option<String>,
}

fn default_database_path() -> String {
    "steel-proxy.db".to_string()
}

fn default_bind_address() -> String {
    "0.0.0.0".to_string()
}

fn default_bind_port() -> u16 {
    25565
}

fn default_public_address() -> String {
    "localhost".to_string()
}

fn default_online_mode() -> bool {
    true
}

fn default_instance_timeout() -> u64 {
    30
}

fn default_image_name() -> String {
    "steel-server".to_string()
}

fn default_memory_limit() -> String {
    "2Gi".to_string()
}

fn default_cpu_limit() -> String {
    "1000m".to_string()
}

fn default_build_timeout() -> u64 {
    15
}

impl ProxyConfig {
    /// Load configuration from a TOML file
    pub fn load(path: &str) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path))?;
        let config: ProxyConfig =
            toml::from_str(&content).context("Failed to parse config file")?;

        // Validate configuration
        if config.backends.is_empty() {
            anyhow::bail!("No backend servers configured");
        }

        if !config.backends.contains_key(&config.default_server) {
            anyhow::bail!(
                "Default server '{}' not found in backends",
                config.default_server
            );
        }

        Ok(config)
    }

    /// Create a default configuration file
    pub fn create_default(path: &str) -> Result<()> {
        let mut backends = HashMap::new();
        backends.insert(
            "lobby".to_string(),
            BackendServer {
                address: "127.0.0.1".to_string(),
                port: 25566,
                description: Some("Lobby server".to_string()),
            },
        );
        backends.insert(
            "survival".to_string(),
            BackendServer {
                address: "127.0.0.1".to_string(),
                port: 25567,
                description: Some("Survival server".to_string()),
            },
        );

        let config = ProxyConfig {
            bind_address: default_bind_address(),
            bind_port: default_bind_port(),
            public_address: default_public_address(),
            online_mode: default_online_mode(),
            database_path: default_database_path(),
            default_server: "lobby".to_string(),
            backends,
            kubernetes: None,
            jenkins: None,
            github: None,
        };

        let toml = toml::to_string_pretty(&config)?;
        fs::write(path, toml)?;

        Ok(())
    }
}
