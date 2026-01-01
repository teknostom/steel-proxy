# Steel Proxy - Minecraft Server Proxy

A Minecraft proxy server for Steel server instances, allowing players to switch between multiple backend servers using the `/server` command.

## Features

- **Multi-Server Support**: Connect multiple Steel backend servers
- **Server Switching**: Use `/server <name>` to switch between servers
- **Transfer Packet**: Uses Minecraft 1.20.5+ Transfer packet for seamless switching
- **Offline Mode**: Currently supports offline mode (online mode coming soon)
- **Bidirectional Packet Forwarding**: Minimal latency with immediate forwarding

## Architecture

```
[Minecraft Client] <---> [Steel Proxy] <---> [Backend Server 1]
                              |
                              +-----------> [Backend Server 2]
```

### How It Works

1. Client connects to proxy (appears as normal Minecraft server)
2. Proxy authenticates player (offline mode - uses client-provided UUID)
3. Proxy connects to default backend server (offline mode)
4. Bidirectional packet forwarding begins
5. When player types `/server <name>`:
   - Proxy intercepts the command
   - Sends Transfer packet to client with proxy's own address
   - Client disconnects and reconnects to proxy
   - Proxy connects to new backend server
   - Forwarding resumes with new server

## Configuration

Create `proxy-config.toml`:

```toml
# Address to bind the proxy to
bind_address = "0.0.0.0"

# Port to listen on
bind_port = 25565

# Enable online mode (currently only offline mode supported)
online_mode = false

# Default server for new connections
default_server = "survival"

# Backend servers
[backends.survival]
address = "127.0.0.1"
port = 25566
description = "Survival Server"

[backends.creative]
address = "127.0.0.1"
port = 25567
description = "Creative Server"
```

## Building

```bash
cargo build --release
```

## Running

1. Start your backend Steel servers in **offline mode** on their configured ports
2. Run the proxy:
   ```bash
   cargo run --release
   ```
3. Connect your Minecraft client to the proxy address (localhost:25565)

## Usage

Once connected:
- You start on the default server (configured in `proxy-config.toml`)
- Type `/server survival` to switch to the survival server
- Type `/server creative` to switch to the creative server

## Current Status

### ✅ Implemented

- [x] CTransfer packet added to steel-protocol
- [x] Proxy crate structure created
- [x] Configuration system (TOML-based)
- [x] Client connection handler (offline mode)
- [x] Backend server connector (offline mode)
- [x] Bidirectional packet forwarding framework
- [x] `/server` command interception
- [x] Transfer packet server switching logic

### 🚧 In Progress

- [ ] Fix remaining compilation errors (packet encoding issues)
- [ ] Test with actual Steel backend servers
- [ ] Handle server switch state persistence

### 📋 Future Enhancements

- [ ] Online mode support (Mojang authentication)
- [ ] Player state caching between servers
- [ ] Connection pooling to backend servers
- [ ] Tab completion for `/server` command
- [ ] Permission system (which servers players can access)
- [ ] MOTD/Player list from all backend servers
- [ ] Fallback server on disconnect
- [ ] Metrics and monitoring

## Known Issues

1. **Compilation errors**: Some packet encoding methods need adjustment for ServerPackets vs ClientPackets
2. **Server switch persistence**: Need to store which server player wants after Transfer packet
3. **No compression/encryption**: Offline mode only, no packet compression or AES encryption

## Technical Details

### Dependencies

- `steel-protocol`: Minecraft protocol implementation
- `steel-utils`: Utility functions and codecs
- `steel-crypto`: Cryptographic operations (not fully utilized yet)
- `tokio`: Async runtime
- `serde`/`toml`: Configuration parsing
- `anyhow`: Error handling

### Packet Flow

**Client → Proxy → Backend:**
1. Proxy receives encrypted packet from client (if online mode)
2. Proxy decrypts packet
3. Check for `/server` command
4. Forward all other packets unchanged to backend

**Backend → Proxy → Client:**
1. Proxy receives packet from backend (unencrypted, offline mode)
2. Forward packet unchanged to client
3. Re-encrypt for client (if online mode)

### Server Switching

When `/server <name>` is detected:
1. Validate server exists in config
2. Send `CTransfer` packet with proxy's address/port
3. Client disconnects
4. Client reconnects to proxy
5. New `ClientHandler` created
6. Connect to requested backend server (TODO: need state storage)
7. Resume packet forwarding

## License

Part of the SteelMC project.
