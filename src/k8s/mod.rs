mod instance;
pub mod lifecycle;

pub use instance::{InstanceState, PrInstance};
pub use lifecycle::run_lifecycle_loop;

use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Service;
use kube::api::{Api, DeleteParams, PostParams};
use kube::Client;
use log::{info, warn};
use parking_lot::Mutex;
use serde_json::json;

use crate::config::{BackendServer, KubernetesConfig};
use crate::proxy::ProxyServer;

/// Manages PR instances in Kubernetes
pub struct K8sManager {
    config: KubernetesConfig,
    /// Active PR instances by PR number
    instances: Mutex<HashMap<u32, PrInstance>>,
    /// Kubernetes client (initialized lazily)
    kube_client: tokio::sync::OnceCell<Client>,
}

impl K8sManager {
    pub fn new(config: KubernetesConfig) -> Self {
        Self {
            config,
            instances: Mutex::new(HashMap::new()),
            kube_client: tokio::sync::OnceCell::new(),
        }
    }

    /// Get or initialize the Kubernetes client
    async fn client(&self) -> Result<&Client> {
        self.kube_client
            .get_or_try_init(|| async {
                Client::try_default()
                    .await
                    .context("Failed to create Kubernetes client from kubeconfig")
            })
            .await
    }

    /// Create a Kubernetes deployment for a PR instance (deletes existing if present)
    pub async fn create_deployment(&self, pr_number: u32) -> Result<String> {
        let client = self.client().await?;
        let deployments: Api<Deployment> = Api::namespaced(client.clone(), &self.config.namespace);

        let name = format!("steel-pr-{}", pr_number);

        // Delete existing deployment if present
        match deployments.delete(&name, &DeleteParams::default()).await {
            Ok(_) => {
                info!("Deleted existing deployment {} before recreating", name);
                // Wait a moment for k8s to process the deletion
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(kube::Error::Api(e)) if e.code == 404 => {
                // Doesn't exist, that's fine
            }
            Err(e) => {
                warn!("Failed to delete existing deployment {}: {}", name, e);
            }
        }
        let image = format!(
            "{}/{}:pr-{}",
            self.config.registry, self.config.image_name, pr_number
        );

        let labels: BTreeMap<String, String> = [
            ("app".to_string(), "steel-server".to_string()),
            ("pr".to_string(), pr_number.to_string()),
        ]
        .into_iter()
        .collect();

        let deployment: Deployment = serde_json::from_value(json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {
                "name": name,
                "namespace": self.config.namespace,
                "labels": labels
            },
            "spec": {
                "replicas": 1,
                "selector": {
                    "matchLabels": labels
                },
                "template": {
                    "metadata": {
                        "labels": labels
                    },
                    "spec": {
                        "containers": [{
                            "name": "steel-server",
                            "image": image,
                            "ports": [{
                                "containerPort": 25565,
                                "name": "minecraft"
                            }],
                            "resources": {
                                "limits": {
                                    "memory": self.config.resources.memory,
                                    "cpu": self.config.resources.cpu
                                },
                                "requests": {
                                    "memory": "512Mi",
                                    "cpu": "250m"
                                }
                            },
                            "readinessProbe": {
                                "tcpSocket": {
                                    "port": 25565
                                },
                                "initialDelaySeconds": 10,
                                "periodSeconds": 5
                            }
                        }]
                    }
                }
            }
        }))?;

        deployments
            .create(&PostParams::default(), &deployment)
            .await
            .context("Failed to create deployment")?;

        info!("Created deployment {} for PR #{}", name, pr_number);
        Ok(name)
    }

    /// Create a Kubernetes service for a PR instance (deletes existing if present)
    pub async fn create_service(&self, pr_number: u32) -> Result<(String, u16)> {
        let client = self.client().await?;
        let services: Api<Service> = Api::namespaced(client.clone(), &self.config.namespace);

        let name = format!("steel-pr-{}", pr_number);

        // Delete existing service if present
        match services.delete(&name, &DeleteParams::default()).await {
            Ok(_) => {
                info!("Deleted existing service {} before recreating", name);
            }
            Err(kube::Error::Api(e)) if e.code == 404 => {
                // Doesn't exist, that's fine
            }
            Err(e) => {
                warn!("Failed to delete existing service {}: {}", name, e);
            }
        }

        let labels: BTreeMap<String, String> = [
            ("app".to_string(), "steel-server".to_string()),
            ("pr".to_string(), pr_number.to_string()),
        ]
        .into_iter()
        .collect();

        let service: Service = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Service",
            "metadata": {
                "name": name,
                "namespace": self.config.namespace
            },
            "spec": {
                "selector": labels,
                "ports": [{
                    "port": 25565,
                    "targetPort": 25565,
                    "name": "minecraft"
                }],
                "type": "NodePort"
            }
        }))?;

        let created = services
            .create(&PostParams::default(), &service)
            .await
            .context("Failed to create service")?;

        // Get the assigned NodePort
        let node_port = created
            .spec
            .as_ref()
            .and_then(|s| s.ports.as_ref())
            .and_then(|ports| ports.first())
            .and_then(|p| p.node_port)
            .ok_or_else(|| anyhow::anyhow!("No NodePort assigned"))?;

        info!(
            "Created service {} for PR #{} on NodePort {}",
            name, pr_number, node_port
        );

        Ok((self.config.node_address.clone(), node_port as u16))
    }

    /// Delete a PR instance's deployment and service
    pub async fn delete_instance(&self, pr_number: u32) -> Result<()> {
        let client = self.client().await?;
        let name = format!("steel-pr-{}", pr_number);

        // Delete deployment
        let deployments: Api<Deployment> = Api::namespaced(client.clone(), &self.config.namespace);
        match deployments.delete(&name, &DeleteParams::default()).await {
            Ok(_) => info!("Deleted deployment {} for PR #{}", name, pr_number),
            Err(kube::Error::Api(e)) if e.code == 404 => {
                warn!("Deployment {} not found, skipping delete", name);
            }
            Err(e) => return Err(e.into()),
        }

        // Delete service
        let services: Api<Service> = Api::namespaced(client.clone(), &self.config.namespace);
        match services.delete(&name, &DeleteParams::default()).await {
            Ok(_) => info!("Deleted service {} for PR #{}", name, pr_number),
            Err(kube::Error::Api(e)) if e.code == 404 => {
                warn!("Service {} not found, skipping delete", name);
            }
            Err(e) => return Err(e.into()),
        }

        Ok(())
    }

    /// Check if a deployment is ready (has available replicas)
    pub async fn is_deployment_ready(&self, deployment_name: &str) -> Result<bool> {
        let client = self.client().await?;
        let deployments: Api<Deployment> = Api::namespaced(client.clone(), &self.config.namespace);

        let deployment = deployments
            .get(deployment_name)
            .await
            .context("Failed to get deployment")?;

        let status = deployment.status.as_ref();
        let available = status.and_then(|s| s.available_replicas).unwrap_or(0);

        Ok(available > 0)
    }

    /// Check if a PR instance exists
    pub fn has_instance(&self, pr_number: u32) -> bool {
        self.instances.lock().contains_key(&pr_number)
    }

    /// Create an instance in Building state with queue URL
    pub fn create_building_instance(&self, pr_number: u32, queue_url: String) {
        let instance = PrInstance::new_with_queue_url(pr_number, queue_url);
        self.instances.lock().insert(pr_number, instance);
        info!("Created building instance for PR #{}", pr_number);
    }

    /// Start building a PR instance
    pub fn start_pr(&self, pr_number: u32) -> Result<(), String> {
        let mut instances = self.instances.lock();

        // Check if already exists
        if let Some(instance) = instances.get(&pr_number) {
            return Err(format!(
                "PR #{} is already {}",
                pr_number,
                instance.state.status_text()
            ));
        }

        // Create new instance in Building state
        let instance = PrInstance::new(pr_number);
        instances.insert(pr_number, instance);

        info!("Started build for PR #{}", pr_number);
        Ok(())
    }

    /// Get the current state of a PR instance
    pub fn get_instance_state(&self, pr_number: u32) -> Option<InstanceState> {
        self.instances.lock().get(&pr_number).map(|i| i.state.clone())
    }

    /// Update instance state (called by lifecycle loop)
    pub fn update_instance_state(&self, pr_number: u32, state: InstanceState) {
        if let Some(instance) = self.instances.lock().get_mut(&pr_number) {
            instance.state = state;
        }
    }

    /// Remove an instance
    pub fn remove_instance(&self, pr_number: u32) {
        self.instances.lock().remove(&pr_number);
    }

    /// Get all instances that need processing
    pub fn get_instances_snapshot(&self) -> Vec<(u32, PrInstance)> {
        self.instances
            .lock()
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect()
    }

    /// Register a ready PR instance as a backend
    pub async fn register_backend(
        &self,
        pr_number: u32,
        address: String,
        port: u16,
        proxy_server: &ProxyServer,
    ) {
        let name = format!("pr-{}", pr_number);
        let backend = BackendServer {
            address,
            port,
            description: Some(format!("PR #{}", pr_number)),
        };

        let backends = proxy_server.backends();
        backends.write().await.add_dynamic(name, backend);
    }

    /// Unregister a PR instance backend
    pub async fn unregister_backend(&self, pr_number: u32, proxy_server: &ProxyServer) {
        let name = format!("pr-{}", pr_number);
        let backends = proxy_server.backends();
        backends.write().await.remove_dynamic(&name);
    }

    /// Get config
    pub fn config(&self) -> &KubernetesConfig {
        &self.config
    }
}
