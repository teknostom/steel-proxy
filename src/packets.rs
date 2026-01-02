use std::io::Write;

use steel_protocol::packet_traits::ClientPacket;
use steel_protocol::packets::game::{
    ArgumentStringTypeBehavior, ArgumentType, CCommands, CommandNode, CommandNodeInfo,
};
use steel_protocol::utils::ConnectionProtocol;
use steel_registry::packets::{config, play};
use steel_utils::codec::VarInt;
use steel_utils::serial::WriteTo;

/// Build command tree based on permission level
pub fn build_commands_for_permission_level(permission_level: u8) -> CCommands {
    let mut nodes = Vec::new();

    // Node indices (we'll build them in order)
    // 0: root
    // 1: server (literal)
    // 2: server_name (argument)
    // If op:
    // 3: start (literal)
    // 4: pr_number for start (argument)
    // 5: status (literal)
    // 6: pr_number for status (argument)

    let mut root_children: Vec<i32> = vec![1]; // server is always available

    if permission_level >= 2 {
        root_children.push(3); // start
        root_children.push(5); // status
    }

    // 0: Root node
    let mut root = CommandNode::new_root();
    root.set_children(root_children);
    nodes.push(root);

    // 1: server (literal, executable to list servers)
    nodes.push(CommandNode::new_literal(
        CommandNodeInfo::new(vec![2]).chain(CommandNodeInfo::new_executable()),
        "server",
    ));

    // 2: server_name (argument, executable)
    nodes.push(CommandNode::new_argument(
        CommandNodeInfo::new_executable(),
        "server_name",
        (
            ArgumentType::String {
                behavior: ArgumentStringTypeBehavior::SingleWord,
            },
            None,
        ),
    ));

    if permission_level >= 2 {
        // 3: start (literal)
        nodes.push(CommandNode::new_literal(
            CommandNodeInfo::new(vec![4]),
            "start",
        ));

        // 4: pr_number for start (argument, executable)
        nodes.push(CommandNode::new_argument(
            CommandNodeInfo::new_executable(),
            "pr_number",
            (ArgumentType::Integer { min: None, max: None }, None),
        ));

        // 5: status (literal)
        nodes.push(CommandNode::new_literal(
            CommandNodeInfo::new(vec![6]),
            "status",
        ));

        // 6: pr_number for status (argument, executable)
        nodes.push(CommandNode::new_argument(
            CommandNodeInfo::new_executable(),
            "pr_number",
            (ArgumentType::Integer { min: None, max: None }, None),
        ));
    }

    CCommands {
        nodes,
        root_index: 0,
    }
}

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
