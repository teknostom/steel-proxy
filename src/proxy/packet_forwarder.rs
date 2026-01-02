use anyhow::{Context, Result};
use log::{debug, error, info, warn};
use std::io::Cursor;
use tokio::io::{BufReader, BufWriter};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::select;

use steel_protocol::packet_reader::TCPNetworkDecoder;
use steel_protocol::packet_traits::EncodedPacket;
use steel_protocol::packet_writer::TCPNetworkEncoder;
use steel_protocol::packets::game::CSystemChat;
use steel_utils::serial::ReadFrom;
use steel_utils::codec::VarInt;

use crate::packets::{build_commands, CTransfer};
use steel_protocol::utils::{ConnectionProtocol, RawPacket};
use steel_registry::packets;
use steel_utils::text::TextComponent;

use super::{BroadcastReceiver, ProxyServer};

/// Result of handling a packet - signals whether to continue or terminate
enum Action {
    Continue,
    Disconnect,
}

pub struct PacketForwarder {
    client_id: u64,
    player_uuid: uuid::Uuid,
    proxy_server: std::sync::Arc<ProxyServer>,

    // Client connection (Option so we can take() them for spawned tasks)
    client_decoder: Option<TCPNetworkDecoder<BufReader<OwnedReadHalf>>>,
    client_encoder: TCPNetworkEncoder<BufWriter<OwnedWriteHalf>>,

    // Backend connection (Option so we can take() them for spawned tasks)
    backend_decoder: Option<TCPNetworkDecoder<BufReader<OwnedReadHalf>>>,
    backend_encoder: TCPNetworkEncoder<BufWriter<OwnedWriteHalf>>,

    // Broadcast receiver for system messages
    broadcast_rx: BroadcastReceiver,

    // State
    current_backend: String,
    current_protocol: ConnectionProtocol,
    backend_compression_threshold: Option<std::num::NonZeroU32>,

    // Player's permission level (0-4, where 2+ is typically OP)
    permission_level: u8,
    // Player's entity ID (needed to identify Entity Event packets for this player)
    entity_id: Option<i32>,
}

impl PacketForwarder {
    pub fn new(
        client_id: u64,
        player_uuid: uuid::Uuid,
        proxy_server: std::sync::Arc<ProxyServer>,
        client_decoder: TCPNetworkDecoder<BufReader<OwnedReadHalf>>,
        client_encoder: TCPNetworkEncoder<BufWriter<OwnedWriteHalf>>,
        backend_decoder: TCPNetworkDecoder<BufReader<OwnedReadHalf>>,
        backend_encoder: TCPNetworkEncoder<BufWriter<OwnedWriteHalf>>,
        current_backend: String,
        backend_compression_threshold: Option<std::num::NonZeroU32>,
    ) -> Self {
        let broadcast_rx = proxy_server.subscribe_broadcast();
        Self {
            client_id,
            player_uuid,
            proxy_server,
            client_decoder: Some(client_decoder),
            client_encoder,
            backend_decoder: Some(backend_decoder),
            backend_encoder,
            broadcast_rx,
            current_backend,
            current_protocol: ConnectionProtocol::Config,
            backend_compression_threshold,
            permission_level: 0,
            entity_id: None,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        info!(
            "[Client {}] Starting packet forwarding to backend: {}",
            self.client_id, self.current_backend
        );

        // Use channels to communicate between reader tasks and the main handler
        let (backend_tx, mut backend_rx) = tokio::sync::mpsc::channel::<Result<RawPacket, String>>(32);
        let (client_tx, mut client_rx) = tokio::sync::mpsc::channel::<Result<RawPacket, String>>(32);

        // Spawn backend reader task (separate task ensures reads aren't cancelled mid-packet)
        let mut backend_decoder = self.backend_decoder.take().expect("backend_decoder already taken");
        let backend_reader = tokio::spawn(async move {
            loop {
                match backend_decoder.get_raw_packet().await {
                    Ok(packet) => {
                        if backend_tx.send(Ok(packet)).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = backend_tx.send(Err(e.to_string())).await;
                        break;
                    }
                }
            }
        });

        // Spawn client reader task (separate task ensures reads aren't cancelled mid-packet)
        let mut client_decoder = self.client_decoder.take().expect("client_decoder already taken");
        let client_reader = tokio::spawn(async move {
            loop {
                match client_decoder.get_raw_packet().await {
                    Ok(packet) => {
                        if client_tx.send(Ok(packet)).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = client_tx.send(Err(e.to_string())).await;
                        break;
                    }
                }
            }
        });

        let result = loop {
            select! {
                // Backend -> Client packets
                Some(result) = backend_rx.recv() => {
                    match result {
                        Ok(packet) => {
                            if let Err(e) = self.handle_backend_packet(packet).await {
                                error!("[Client {}] Error handling backend packet: {}", self.client_id, e);
                                break Err(e);
                            }
                        }
                        Err(e) => {
                            warn!("[Client {}] Backend disconnected: {}", self.client_id, e);
                            break Err(anyhow::anyhow!("Backend read failed: {}", e));
                        }
                    }
                }

                // Client -> Backend packets
                Some(result) = client_rx.recv() => {
                    match result {
                        Ok(packet) => {
                            match self.handle_client_packet(packet).await {
                                Ok(Action::Continue) => {}
                                Ok(Action::Disconnect) => {
                                    // Transfer packet sent - wait for client to disconnect
                                    // Continue forwarding backend packets until client closes connection
                                    break self.drain_until_client_disconnects(
                                        &mut backend_rx,
                                        &mut client_rx,
                                    ).await;
                                }
                                Err(e) => {
                                    error!("[Client {}] Error handling client packet: {}", self.client_id, e);
                                    break Err(e);
                                }
                            }
                        }
                        Err(_) => {
                            info!("[Client {}] Client disconnected", self.client_id);
                            break Ok(());
                        }
                    }
                }

                // Broadcast messages from background tasks
                result = self.broadcast_rx.recv() => {
                    if let Ok(message) = result {
                        if let Err(e) = self.send_system_message(&message).await {
                            warn!("[Client {}] Failed to send broadcast: {}", self.client_id, e);
                        }
                    }
                }

                else => {
                    info!("[Client {}] All channels closed", self.client_id);
                    break Ok(());
                }
            }
        };

        // Abort reader tasks
        backend_reader.abort();
        client_reader.abort();

        result
    }

    /// After sending a Transfer packet, continue forwarding backend packets until the client disconnects.
    /// This ensures the client receives any in-flight packets before processing the Transfer.
    async fn drain_until_client_disconnects(
        &mut self,
        backend_rx: &mut tokio::sync::mpsc::Receiver<Result<RawPacket, String>>,
        client_rx: &mut tokio::sync::mpsc::Receiver<Result<RawPacket, String>>,
    ) -> Result<()> {
        let timeout = tokio::time::sleep(tokio::time::Duration::from_secs(2));
        tokio::pin!(timeout);

        loop {
            select! {
                // Continue forwarding backend packets
                Some(result) = backend_rx.recv() => {
                    if let Ok(packet) = result {
                        // Best effort forward, ignore errors (client may have disconnected)
                        let _ = self.forward_to_client(packet).await;
                    }
                }

                // Wait for client to disconnect
                Some(result) = client_rx.recv() => {
                    if result.is_err() {
                        info!("[Client {}] Client disconnected after transfer", self.client_id);
                        return Ok(());
                    }
                    // Ignore any packets from client, they're switching servers
                }

                // Timeout - force disconnect
                _ = &mut timeout => {
                    info!("[Client {}] Transfer timeout, closing connection", self.client_id);
                    return Ok(());
                }
            }
        }
    }

    async fn handle_client_packet(&mut self, packet: RawPacket) -> Result<Action> {
        // Check for chat command in Play state (both signed and unsigned)
        if self.current_protocol == ConnectionProtocol::Play
            && (packet.id == packets::play::S_CHAT_COMMAND || packet.id == packets::play::S_CHAT_COMMAND_SIGNED)
        {
            // Parse command string from payload
            // Unsigned (ID 6): just VarInt-prefixed string
            // Signed (ID 7): VarInt-prefixed string + timestamp + salt + signatures...
            // Both start with the command string, so we can parse just that part
            if let Ok(command) = self.parse_command_from_payload(&packet.payload) {
                // Check if it's a server command
                if command == "server" {
                    // No argument - list all servers
                    self.list_servers().await?;
                    return Ok(Action::Continue);
                } else if let Some(server_name) = command.strip_prefix("server ") {
                    let server_name = server_name.trim();
                    info!("[Client {}] Switching to server: {}", self.client_id, server_name);
                    return self.switch_server(server_name).await;
                } else if let Some(pr_arg) = command.strip_prefix("start ") {
                    let pr_arg = pr_arg.trim();
                    self.handle_start_command(pr_arg).await?;
                    return Ok(Action::Continue);
                } else if let Some(pr_arg) = command.strip_prefix("status ") {
                    let pr_arg = pr_arg.trim();
                    self.handle_status_command(pr_arg).await?;
                    return Ok(Action::Continue);
                }
            }
        }

        // Track protocol state changes
        if packet.id == packets::config::S_FINISH_CONFIGURATION {
            self.current_protocol = ConnectionProtocol::Play;
        }

        // Forward packet to backend
        self.forward_to_backend(packet).await?;
        Ok(Action::Continue)
    }

    async fn handle_backend_packet(&mut self, packet: RawPacket) -> Result<()> {
        // Track permission level and inject commands
        if self.current_protocol == ConnectionProtocol::Play {
            // Login packet (48) - contains entity ID
            if packet.id == packets::play::C_LOGIN {
                // Entity ID is the first field (i32)
                if packet.payload.len() >= 4 {
                    let entity_id = i32::from_be_bytes([
                        packet.payload[0],
                        packet.payload[1],
                        packet.payload[2],
                        packet.payload[3],
                    ]);
                    self.entity_id = Some(entity_id);
                    info!(
                        "[Client {}] Entity ID: {}",
                        self.client_id, entity_id
                    );
                }
            }
            // Entity Event packet (34) - can contain permission level updates
            else if packet.id == packets::play::C_ENTITY_EVENT {
                // Format: entity_id (i32) + status (u8)
                if packet.payload.len() >= 5 {
                    let entity_id = i32::from_be_bytes([
                        packet.payload[0],
                        packet.payload[1],
                        packet.payload[2],
                        packet.payload[3],
                    ]);
                    let status = packet.payload[4];

                    // Check if this is for our player and is a permission level update
                    if self.entity_id == Some(entity_id) && (24..=28).contains(&status) {
                        self.permission_level = status - 24;
                        debug!(
                            "[Client {}] Permission level updated to {}",
                            self.client_id, self.permission_level
                        );
                    }
                }
            }
            // Commands packet (16) - replace with our own command tree
            else if packet.id == packets::play::C_COMMANDS {
                debug!(
                    "[Client {}] Intercepting commands packet, sending proxy command tree",
                    self.client_id
                );
                return self.send_commands_packet().await;
            }
        }

        // Forward packet to client
        self.forward_to_client(packet).await
    }

    async fn send_commands_packet(&mut self) -> Result<()> {
        let commands = build_commands();
        let encoded =
            EncodedPacket::from_bare(commands, None, ConnectionProtocol::Play).await?;
        self.client_encoder.write_packet(&encoded).await?;
        Ok(())
    }

    async fn forward_to_backend(&mut self, raw_packet: RawPacket) -> Result<()> {
        use steel_utils::FrontVec;
        use steel_utils::codec::VarInt;
        use steel_utils::serial::WriteTo;

        // Re-encode: packet ID + payload
        let mut buf = FrontVec::new(6);
        VarInt(raw_packet.id).write(&mut buf)?;
        buf.extend_from_slice(&raw_packet.payload);

        // Backend has compression enabled, so use compression info
        let compression = self.backend_compression_threshold.map(|threshold| {
            steel_protocol::packet_traits::CompressionInfo {
                threshold,
                level: 4,
            }
        });

        let encoded = EncodedPacket::from_data(buf, compression).await?;
        self.backend_encoder
            .write_packet(&encoded)
            .await
            .context("Failed to send packet to backend")?;
        Ok(())
    }

    async fn forward_to_client(&mut self, raw_packet: RawPacket) -> Result<()> {
        use steel_utils::FrontVec;
        use steel_utils::codec::VarInt;
        use steel_utils::serial::WriteTo;

        // Re-encode: packet ID + payload
        let mut buf = FrontVec::new(6);
        VarInt(raw_packet.id).write(&mut buf)?;
        buf.extend_from_slice(&raw_packet.payload);

        // Client connection has NO compression - send uncompressed packets
        let encoded = EncodedPacket::from_data(buf, None).await?;
        self.client_encoder
            .write_packet(&encoded)
            .await
            .context("Failed to send packet to client")?;
        Ok(())
    }

    fn parse_command_from_payload(&self, payload: &[u8]) -> Result<String> {
        let mut cursor = Cursor::new(payload);
        // Read VarInt-prefixed string (command without leading slash)
        let len = VarInt::read(&mut cursor)?.0 as usize;
        let mut buf = vec![0u8; len];
        std::io::Read::read_exact(&mut cursor, &mut buf)?;
        Ok(String::from_utf8(buf)?)
    }

    async fn list_servers(&mut self) -> Result<()> {
        let mut message = String::from("Available servers:");
        {
            let backends_arc = self.proxy_server.backends();
            let backends = backends_arc.read().await;
            for (name, backend) in backends.iter() {
                let current = if name == &self.current_backend {
                    " (current)"
                } else {
                    ""
                };
                let desc = backend.description.as_deref().unwrap_or("");
                if desc.is_empty() {
                    message.push_str(&format!("\n  - {name}{current}"));
                } else {
                    message.push_str(&format!("\n  - {name}: {desc}{current}"));
                }
            }
        }
        self.send_system_message(&message).await
    }

    async fn send_system_message(&mut self, message: &str) -> Result<()> {
        let chat = CSystemChat::new(TextComponent::from(message.to_string()), false);
        let encoded = EncodedPacket::from_bare(chat, None, ConnectionProtocol::Play).await?;
        self.client_encoder.write_packet(&encoded).await?;
        Ok(())
    }

    async fn switch_server(&mut self, server_name: &str) -> Result<Action> {
        // Check if server exists
        let server_exists = {
            let backends_arc = self.proxy_server.backends();
            let backends = backends_arc.read().await;
            backends.contains(server_name)
        };

        if !server_exists {
            warn!(
                "[Client {}] Unknown server: {}",
                self.client_id, server_name
            );
            self.send_system_message(&format!("Unknown server: {server_name}"))
                .await?;
            return Ok(Action::Continue);
        }

        info!(
            "[Client {}] {} -> {}",
            self.client_id, self.current_backend, server_name
        );

        // Store the target server for when the client reconnects
        self.proxy_server
            .set_target_server(self.player_uuid, server_name.to_string());

        // Send Transfer packet to client with proxy's public address
        let config = self.proxy_server.config();
        let transfer = CTransfer::new(config.public_address.clone(), config.bind_port as i32);

        let encoded = EncodedPacket::from_bare(transfer, None, ConnectionProtocol::Play).await?;
        self.client_encoder.write_packet(&encoded).await?;

        // Signal to drain backend packets until client disconnects
        Ok(Action::Disconnect)
    }

    async fn handle_start_command(&mut self, pr_arg: &str) -> Result<()> {
        // Parse PR number
        let pr_number: u32 = match pr_arg.parse() {
            Ok(n) => n,
            Err(_) => {
                self.send_system_message("§cUsage: /start <pr_number>")
                    .await?;
                return Ok(());
            }
        };

        info!(
            "[Client {}] Starting PR #{} instance",
            self.client_id, pr_number
        );

        // Call proxy server to start the PR
        match self.proxy_server.start_pr(pr_number).await {
            Ok(()) => {
                // Success message is sent via broadcast in start_pr
            }
            Err(e) => {
                self.send_system_message(&format!("§cFailed to start PR #{}: {}", pr_number, e))
                    .await?;
            }
        }

        Ok(())
    }

    async fn handle_status_command(&mut self, pr_arg: &str) -> Result<()> {
        // Parse PR number
        let pr_number: u32 = match pr_arg.parse() {
            Ok(n) => n,
            Err(_) => {
                self.send_system_message("§cUsage: /status <pr_number>")
                    .await?;
                return Ok(());
            }
        };

        // Get instance status
        if let Some(k8s) = self.proxy_server.k8s_manager() {
            if let Some(state) = k8s.get_instance_state(pr_number) {
                let status_msg = match state {
                    crate::k8s::InstanceState::Building { .. } => {
                        format!("§e[Status] PR #{} is building...", pr_number)
                    }
                    crate::k8s::InstanceState::Deploying { .. } => {
                        format!("§e[Status] PR #{} is deploying...", pr_number)
                    }
                    crate::k8s::InstanceState::Starting { .. } => {
                        format!("§e[Status] PR #{} is starting...", pr_number)
                    }
                    crate::k8s::InstanceState::Ready { .. } => {
                        format!("§a[Status] PR #{} is ready! Use §e/server pr-{}§a to connect.", pr_number, pr_number)
                    }
                    crate::k8s::InstanceState::ShuttingDown { .. } => {
                        format!("§c[Status] PR #{} is shutting down...", pr_number)
                    }
                    crate::k8s::InstanceState::Failed { reason } => {
                        format!("§c[Status] PR #{} failed: {}", pr_number, reason)
                    }
                };
                self.send_system_message(&status_msg).await?;
            } else {
                // Check if it exists as a backend (already running)
                let backends_arc = self.proxy_server.backends();
                let backends = backends_arc.read().await;
                let backend_name = format!("pr-{}", pr_number);
                if backends.contains(&backend_name) {
                    self.send_system_message(&format!(
                        "§a[Status] PR #{} is running. Use §e/server pr-{}§a to connect.",
                        pr_number, pr_number
                    )).await?;
                } else {
                    self.send_system_message(&format!(
                        "§7[Status] PR #{} is not running. Use §e/start {}§7 to start it.",
                        pr_number, pr_number
                    )).await?;
                }
            }
        } else {
            self.send_system_message("§cKubernetes not configured").await?;
        }

        Ok(())
    }
}
