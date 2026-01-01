use std::io::Write;

use steel_protocol::packet_traits::ClientPacket;
use steel_protocol::utils::ConnectionProtocol;
use steel_registry::packets::{config, play};
use steel_utils::codec::VarInt;
use steel_utils::serial::WriteTo;

/// Server -> Client: Transfer to another server
///
/// Tells the client to disconnect and reconnect to a different server.
/// Can be sent during Config or Play phase.
#[derive(Clone, Debug)]
pub struct CTransfer {
    pub host: String,
    pub port: i32,
}

impl CTransfer {
    pub fn new(host: impl Into<String>, port: i32) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }
}

impl WriteTo for CTransfer {
    fn write(&self, writer: &mut impl Write) -> std::io::Result<()> {
        // Write host as VarInt-prefixed string
        VarInt(self.host.len() as i32).write(writer)?;
        writer.write_all(self.host.as_bytes())?;
        // Write port as VarInt
        VarInt(self.port).write(writer)?;
        Ok(())
    }
}

impl ClientPacket for CTransfer {
    fn get_id(&self, protocol: ConnectionProtocol) -> Option<i32> {
        match protocol {
            ConnectionProtocol::Config => Some(config::C_TRANSFER),
            ConnectionProtocol::Play => Some(play::C_TRANSFER),
            _ => None,
        }
    }
}
