use std::sync::Arc;
use std::time::{Duration, Instant};

use log::{error, info, warn};
use tokio::time::interval;

use crate::jenkins::{BuildStatus, JenkinsClient};
use crate::proxy::ProxyServer;

use super::{InstanceState, K8sManager};

/// Runs the lifecycle management loop for PR instances
pub async fn run_lifecycle_loop(
    proxy_server: Arc<ProxyServer>,
    k8s_manager: Arc<K8sManager>,
    jenkins_client: Arc<JenkinsClient>,
) {
    let mut ticker = interval(Duration::from_secs(10));

    loop {
        ticker.tick().await;

        // Get snapshot of all instances
        let instances = k8s_manager.get_instances_snapshot();

        for (pr_number, instance) in instances {
            let commit_hash_short = instance.commit_hash_short.clone();

            match &instance.state {
                InstanceState::Building {
                    started_at,
                    build_url,
                } => {
                    handle_building(
                        pr_number,
                        &commit_hash_short,
                        started_at,
                        build_url.as_deref(),
                        &proxy_server,
                        &k8s_manager,
                        &jenkins_client,
                    )
                    .await;
                }

                InstanceState::Deploying { image_tag } => {
                    handle_deploying(
                        pr_number,
                        image_tag,
                        &proxy_server,
                        &k8s_manager,
                    )
                    .await;
                }

                InstanceState::Starting {
                    deployment_name,
                    address,
                    port,
                    started_at,
                } => {
                    handle_starting(
                        pr_number,
                        &instance.commit_hash,
                        &commit_hash_short,
                        deployment_name,
                        address,
                        *port,
                        started_at,
                        &proxy_server,
                        &k8s_manager,
                    )
                    .await;
                }

                InstanceState::Ready {
                    deployment_name,
                    shutdown_at,
                    ..
                } => {
                    handle_ready(
                        pr_number,
                        deployment_name,
                        shutdown_at,
                        &proxy_server,
                        &k8s_manager,
                    )
                    .await;
                }

                InstanceState::ShuttingDown { deployment_name } => {
                    handle_shutting_down(pr_number, deployment_name, &proxy_server, &k8s_manager)
                        .await;
                }

                InstanceState::Failed { reason } => {
                    // Clean up failed instances after some time
                    warn!("PR #{} failed: {}", pr_number, reason);
                    k8s_manager.remove_instance(pr_number);
                }
            }
        }
    }
}

/// Handle a building instance - poll Jenkins for build status
async fn handle_building(
    pr_number: u32,
    commit_hash_short: &str,
    started_at: &Instant,
    queue_url: Option<&str>,
    proxy_server: &ProxyServer,
    k8s_manager: &K8sManager,
    jenkins_client: &JenkinsClient,
) {
    // Check for build timeout
    let timeout = jenkins_client.build_timeout();
    if started_at.elapsed() > timeout {
        error!("PR #{} build timed out after {:?}", pr_number, timeout);
        proxy_server.broadcast(&format!(
            "§c[Proxy] PR #{} build timed out after {} minutes",
            pr_number,
            timeout.as_secs() / 60
        ));
        k8s_manager.update_instance_state(
            pr_number,
            InstanceState::Failed {
                reason: "Build timeout".to_string(),
            },
        );
        return;
    }

    // Get build URL from queue if we don't have it yet
    let Some(queue_url) = queue_url else {
        return;
    };

    // Try to get the actual build URL
    let build_url = match jenkins_client.get_build_url(queue_url).await {
        Ok(Some(url)) => url,
        Ok(None) => {
            // Still queued
            return;
        }
        Err(e) => {
            warn!("Failed to get build URL for PR #{}: {}", pr_number, e);
            return;
        }
    };

    // Poll build status
    match jenkins_client.get_build_status(&build_url).await {
        Ok(BuildStatus::Success) => {
            info!("PR #{} build completed successfully", pr_number);
            proxy_server.broadcast(&format!(
                "§a[Proxy] PR #{} build completed! Deploying...",
                pr_number
            ));

            // Transition to Deploying state with commit-specific image tag
            let image_tag = format!("pr-{}-{}", pr_number, commit_hash_short);
            k8s_manager.update_instance_state(
                pr_number,
                InstanceState::Deploying { image_tag },
            );
        }
        Ok(BuildStatus::Failed { reason }) => {
            error!("PR #{} build failed: {}", pr_number, reason);
            proxy_server.broadcast(&format!(
                "§c[Proxy] PR #{} build failed: {}",
                pr_number, reason
            ));
            k8s_manager.update_instance_state(pr_number, InstanceState::Failed { reason });
        }
        Ok(BuildStatus::Building | BuildStatus::Queued) => {
            // Still building, nothing to do
        }
        Ok(BuildStatus::NotFound) => {
            warn!("PR #{} build not found", pr_number);
            k8s_manager.update_instance_state(
                pr_number,
                InstanceState::Failed {
                    reason: "Build not found".to_string(),
                },
            );
        }
        Err(e) => {
            warn!("Failed to get build status for PR #{}: {}", pr_number, e);
        }
    }
}

/// Handle deploying state - create k8s deployment
async fn handle_deploying(
    pr_number: u32,
    image_tag: &str,
    proxy_server: &ProxyServer,
    k8s_manager: &K8sManager,
) {
    // Create deployment with the commit-specific image tag
    match k8s_manager.create_deployment(pr_number, image_tag).await {
        Ok(deployment_name) => {
            info!("Created deployment {} for PR #{}", deployment_name, pr_number);

            // Create service
            match k8s_manager.create_service(pr_number).await {
                Ok((address, port)) => {
                    // Transition to Starting state
                    k8s_manager.update_instance_state(
                        pr_number,
                        InstanceState::Starting {
                            deployment_name,
                            address,
                            port,
                            started_at: Instant::now(),
                        },
                    );
                }
                Err(e) => {
                    error!("Failed to create service for PR #{}: {}", pr_number, e);
                    proxy_server.broadcast(&format!(
                        "§c[Proxy] Failed to create service for PR #{}: {}",
                        pr_number, e
                    ));
                    k8s_manager.update_instance_state(
                        pr_number,
                        InstanceState::Failed {
                            reason: format!("Service creation failed: {}", e),
                        },
                    );
                }
            }
        }
        Err(e) => {
            error!("Failed to create deployment for PR #{}: {}", pr_number, e);
            proxy_server.broadcast(&format!(
                "§c[Proxy] Failed to deploy PR #{}: {}",
                pr_number, e
            ));
            k8s_manager.update_instance_state(
                pr_number,
                InstanceState::Failed {
                    reason: format!("Deployment failed: {}", e),
                },
            );
        }
    }
}

/// Handle starting state - wait for pod readiness
async fn handle_starting(
    pr_number: u32,
    commit_hash: &str,
    commit_hash_short: &str,
    deployment_name: &str,
    address: &str,
    port: u16,
    started_at: &Instant,
    proxy_server: &ProxyServer,
    k8s_manager: &K8sManager,
) {
    // Check for startup timeout (5 minutes)
    let timeout = Duration::from_secs(300);
    if started_at.elapsed() > timeout {
        error!("PR #{} startup timed out", pr_number);
        proxy_server.broadcast(&format!(
            "§c[Proxy] PR #{} startup timed out",
            pr_number
        ));
        k8s_manager.update_instance_state(
            pr_number,
            InstanceState::ShuttingDown {
                deployment_name: deployment_name.to_string(),
            },
        );
        return;
    }

    // Check if deployment is ready
    match k8s_manager.is_deployment_ready(deployment_name).await {
        Ok(true) => {
            info!("PR #{} deployment is ready", pr_number);

            // Register as backend using NodePort address
            k8s_manager
                .register_backend(pr_number, address.to_string(), port, proxy_server)
                .await;

            // Calculate shutdown time
            let timeout_minutes = k8s_manager.config().instance_timeout_minutes;
            let shutdown_at = Instant::now() + Duration::from_secs(timeout_minutes * 60);

            // Transition to Ready state
            k8s_manager.update_instance_state(
                pr_number,
                InstanceState::Ready {
                    deployment_name: deployment_name.to_string(),
                    address: address.to_string(),
                    port,
                    ready_at: Instant::now(),
                    shutdown_at,
                },
            );

            // Persist to database so we can skip builds for this commit next time
            let image_tag = format!("pr-{}-{}", pr_number, commit_hash_short);
            if let Err(e) = crate::db::PrBuild::upsert(
                k8s_manager.db_pool(),
                pr_number,
                commit_hash,
                commit_hash_short,
                &image_tag,
                "Ready",
                None,
            ).await {
                warn!("Failed to persist PR #{} build to database: {}", pr_number, e);
            }

            proxy_server.broadcast(&format!(
                "§a[Proxy] PR #{} is ready! Use §e/server pr-{}§a to connect.",
                pr_number, pr_number
            ));
        }
        Ok(false) => {
            // Still starting
        }
        Err(e) => {
            warn!(
                "Failed to check deployment status for PR #{}: {}",
                pr_number, e
            );
        }
    }
}

/// Handle ready state - check for shutdown timer
async fn handle_ready(
    pr_number: u32,
    deployment_name: &str,
    shutdown_at: &Instant,
    proxy_server: &ProxyServer,
    k8s_manager: &K8sManager,
) {
    if Instant::now() >= *shutdown_at {
        info!("PR #{} shutdown timer expired", pr_number);
        proxy_server.broadcast(&format!(
            "§e[Proxy] PR #{} is shutting down (timeout expired)",
            pr_number
        ));

        // Unregister backend
        k8s_manager.unregister_backend(pr_number, proxy_server).await;

        // Transition to ShuttingDown state
        k8s_manager.update_instance_state(
            pr_number,
            InstanceState::ShuttingDown {
                deployment_name: deployment_name.to_string(),
            },
        );
    }
}

/// Handle shutting down state - delete k8s resources
async fn handle_shutting_down(
    pr_number: u32,
    _deployment_name: &str,
    proxy_server: &ProxyServer,
    k8s_manager: &K8sManager,
) {
    match k8s_manager.delete_instance(pr_number).await {
        Ok(()) => {
            info!("Deleted PR #{} resources", pr_number);
            k8s_manager.remove_instance(pr_number);
        }
        Err(e) => {
            error!("Failed to delete PR #{} resources: {}", pr_number, e);
            proxy_server.broadcast(&format!(
                "§c[Proxy] Failed to clean up PR #{}: {}",
                pr_number, e
            ));
            // Still remove from tracking
            k8s_manager.remove_instance(pr_number);
        }
    }
}
