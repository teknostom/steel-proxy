use anyhow::{Context, Result};
use log::{info, warn};
use std::io::Cursor;
use std::net::SocketAddr;
use tokio::io::{BufReader, BufWriter};
use tokio::net::TcpStream;

use steel_protocol::packet_reader::TCPNetworkDecoder;
use steel_protocol::packet_traits::{EncodedPacket, ServerPacket};
use steel_protocol::packet_writer::TCPNetworkEncoder;
use steel_protocol::packets::handshake::{ClientIntent, SClientIntention};
use steel_protocol::packets::login::{CLoginFinished, SHello};
use steel_protocol::packets::status::{CStatusResponse, CPongResponse, SPingRequest};
use steel_protocol::utils::ConnectionProtocol;
use steel_registry::packets;

use super::backend_connector::BackendConnector;
use super::packet_forwarder::PacketForwarder;
use super::ProxyServer;

pub struct ClientHandler {
    client_id: u64,
    stream: TcpStream,
    proxy_server: std::sync::Arc<ProxyServer>,
}

impl ClientHandler {
    pub fn new(
        client_id: u64,
        stream: TcpStream,
        _addr: SocketAddr,
        proxy_server: std::sync::Arc<ProxyServer>,
    ) -> Self {
        Self {
            client_id,
            stream,
            proxy_server,
        }
    }

    pub async fn run(self) -> Result<()> {
        let (read, write) = self.stream.into_split();
        let mut decoder = TCPNetworkDecoder::new(BufReader::new(read));
        let mut encoder = TCPNetworkEncoder::new(BufWriter::new(write));

        let mut protocol = ConnectionProtocol::Handshake;
        let mut username: Option<String> = None;
        let mut player_uuid: Option<uuid::Uuid> = None;

        // Handle handshake and login
        loop {
            let packet = decoder.get_raw_packet().await?;

            match protocol {
                ConnectionProtocol::Handshake => {
                    if packet.id == packets::handshake::S_INTENTION {
                        let handshake =
                            SClientIntention::read_packet(&mut Cursor::new(packet.payload))?;

                        match handshake.intention {
                            ClientIntent::STATUS => {
                                protocol = ConnectionProtocol::Status;
                            }
                            ClientIntent::LOGIN | ClientIntent::TRANSFER => {
                                protocol = ConnectionProtocol::Login;
                            }
                        }
                    }
                }

                ConnectionProtocol::Status => {
                    if packet.id == packets::status::S_STATUS_REQUEST {
                        // Send status response
                        use steel_protocol::packets::status::{Status, Version, Players};

                        let status = Status {
                            description: "Steel Proxy Server (Offline Mode)",
                            players: Some(Players {
                                max: 100,
                                online: 0,
                                sample: vec![],
                            }),
                            version: Some(Version {
                                name: "1.21.11",
                                protocol: 774,
                            }),
                            favicon: None,
                            enforce_secure_chat: false,
                        };

                        let response = CStatusResponse::new(status);

                        let encoded = EncodedPacket::from_bare(
                            response,
                            None,
                            ConnectionProtocol::Status,
                        )
                        .await?;
                        encoder.write_packet(&encoded).await?;
                    } else if packet.id == packets::status::S_PING_REQUEST {
                        // Echo ping
                        let ping = SPingRequest::read_packet(&mut Cursor::new(packet.payload))?;
                        let response = CPongResponse {
                            time: ping.time,
                        };

                        let encoded = EncodedPacket::from_bare(
                            response,
                            None,
                            ConnectionProtocol::Status,
                        )
                        .await?;
                        encoder.write_packet(&encoded).await?;

                        // Status complete, disconnect
                        return Ok(());
                    }
                }

                ConnectionProtocol::Login => {
                    if packet.id == packets::login::S_HELLO {
                        let hello = SHello::read_packet(&mut Cursor::new(packet.payload))?;
                        username = Some(hello.name.clone());
                        player_uuid = Some(hello.profile_id);

                        info!("[Client {}] Player: {}", self.client_id, hello.name);

                        // Offline mode - send login success immediately
                        let success = CLoginFinished {
                            uuid: hello.profile_id,
                            name: &hello.name,
                            properties: &[],
                        };

                        let encoded = EncodedPacket::from_bare(
                            success,
                            None,
                            ConnectionProtocol::Login,
                        )
                        .await?;
                        encoder.write_packet(&encoded).await?;
                    } else if packet.id == packets::login::S_LOGIN_ACKNOWLEDGED {
                        // Login complete, transition to config
                        protocol = ConnectionProtocol::Config;

                        // Check if player has a pending server switch
                        let backend_name = if let Some(target_server) =
                            self.proxy_server.get_target_server(player_uuid.unwrap())
                        {
                            info!(
                                "[Client {}] Reconnecting to: {}",
                                self.client_id, target_server
                            );
                            target_server
                        } else {
                            self.proxy_server.config().default_server.clone()
                        };

                        // Look up backend from shared registry
                        let (backend_address, backend_port) = {
                            let backends_arc = self.proxy_server.backends();
                            let backends = backends_arc.read().await;
                            let backend = backends
                                .get(&backend_name)
                                .context("Backend server not found")?;
                            (backend.address.clone(), backend.port)
                        };

                        // Connect to backend server
                        let backend_connector = BackendConnector::new(
                            backend_address,
                            backend_port,
                            username.clone().unwrap(),
                            player_uuid.unwrap(),
                        );

                        let (backend_decoder, backend_encoder, backend_compression) =
                            backend_connector.connect().await?;

                        // Start packet forwarding
                        let forwarder = PacketForwarder::new(
                            self.client_id,
                            player_uuid.unwrap(),
                            self.proxy_server.clone(),
                            decoder,
                            encoder,
                            backend_decoder,
                            backend_encoder,
                            backend_name.clone(),
                            backend_compression,
                        );

                        return forwarder.run().await;
                    }
                }

                _ => {
                    warn!("[Client {}] Unexpected protocol state: {:?}", self.client_id, protocol);
                    return Ok(());
                }
            }
        }
    }
}
