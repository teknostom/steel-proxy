use anyhow::{Context, Result};
use log::info;
use std::io::Cursor;
use tokio::io::{BufReader, BufWriter};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;

use steel_protocol::packet_reader::TCPNetworkDecoder;
use steel_protocol::packet_traits::{EncodedPacket, ServerPacket};
use steel_protocol::packet_writer::TCPNetworkEncoder;
use steel_protocol::packets::handshake::ClientIntent;
use steel_registry::packets;

pub struct BackendConnector {
    address: String,
    port: u16,
    username: String,
    uuid: uuid::Uuid,
}

impl BackendConnector {
    pub fn new(address: String, port: u16, username: String, uuid: uuid::Uuid) -> Self {
        Self {
            address,
            port,
            username,
            uuid,
        }
    }

    pub async fn connect(
        self,
    ) -> Result<(
        TCPNetworkDecoder<BufReader<OwnedReadHalf>>,
        TCPNetworkEncoder<BufWriter<OwnedWriteHalf>>,
        Option<std::num::NonZeroU32>, // compression threshold if enabled
    )> {
        // Connect to backend server
        let stream = TcpStream::connect(format!("{}:{}", self.address, self.port))
            .await
            .context("Failed to connect to backend server")?;

        let (read, write) = stream.into_split();
        let mut decoder = TCPNetworkDecoder::new(BufReader::new(read));
        let mut encoder = TCPNetworkEncoder::new(BufWriter::new(write));

        // Send handshake - manually encode since SClientIntention is ServerPacket not ClientPacket
        use steel_utils::serial::WriteTo;
        use steel_utils::codec::VarInt;
        use steel_utils::FrontVec;

        let mut buf = FrontVec::new(6);
        // Write packet ID for handshake intention
        VarInt(packets::handshake::S_INTENTION).write(&mut buf)?;
        // Write protocol version
        VarInt(774).write(&mut buf)?;
        // Write hostname (prefixed with VarInt length)
        VarInt(self.address.len() as i32).write(&mut buf)?;
        buf.extend_from_slice(self.address.as_bytes());
        // Write port
        self.port.write(&mut buf)?;
        // Write intention
        VarInt(ClientIntent::LOGIN as i32).write(&mut buf)?;

        let encoded = EncodedPacket::from_data(buf, None).await?;
        encoder.write_packet(&encoded).await?;

        // Send login start (offline mode - no encryption) - manually encode ServerPacket
        let mut buf = FrontVec::new(6);
        // Write packet ID for login hello
        VarInt(packets::login::S_HELLO).write(&mut buf)?;
        // Write username (prefixed with VarInt length, max 16)
        VarInt(self.username.len() as i32).write(&mut buf)?;
        buf.extend_from_slice(self.username.as_bytes());
        // Write UUID
        self.uuid.write(&mut buf)?;

        let encoded = EncodedPacket::from_data(buf, None).await?;
        encoder.write_packet(&encoded).await?;

        // Wait for response from backend (could be compression, encryption request, or login success)
        let mut compression_threshold = None;
        loop {
            let response = decoder.get_raw_packet().await?;

            match response.id {
                packets::login::C_LOGIN_COMPRESSION => {
                    // Backend wants compression - read threshold and enable it
                    use steel_utils::serial::ReadFrom;
                    let threshold = steel_utils::codec::VarInt::read(&mut std::io::Cursor::new(&response.payload))?;

                    if threshold.0 > 0 {
                        let threshold_nz = std::num::NonZeroU32::new(threshold.0 as u32).unwrap();
                        decoder.set_compression(threshold_nz);
                        compression_threshold = Some(threshold_nz);
                    }
                    // Continue reading next packet
                }
                packets::login::C_LOGIN_FINISHED => {
                    break; // Exit loop, we got what we need
                }
                _ => {
                    anyhow::bail!("Unexpected packet from backend during login: ID={}", response.id);
                }
            }
        }

        // Send LoginAcknowledged to backend to complete login
        // NOTE: Must use compression if backend requested it!
        let mut buf = FrontVec::new(6);
        VarInt(packets::login::S_LOGIN_ACKNOWLEDGED).write(&mut buf)?;

        let compression = compression_threshold.map(|threshold| {
            steel_protocol::packet_traits::CompressionInfo {
                threshold,
                level: 4,
            }
        });

        let encoded = EncodedPacket::from_data(buf, compression).await?;
        encoder.write_packet(&encoded).await?;

        Ok((decoder, encoder, compression_threshold))
    }
}
