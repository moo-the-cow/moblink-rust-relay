# Moblink Rust Relay

Use spare devices as extra SRTLA bonding connections. The same functionality is part of [Moblin](https://github.com/eerimoq/moblin) on iOS.

Originally inspired by the Moblink Kotlin/Android code.

## Features

- **WebSocket Connection**: Connects to Moblin via WebSocket (e.g., `wss://...`)  
- **Auth Handling**: Implements the same challenge–response authentication logic as the Android client  
- **UDP Relay**: Forwards UDP packets between the remote streamer and a local destination.  

## Requirements

- **Rust** (stable, e.g., 1.70+)
- **Cargo** (for building)

## Usage

```bash
# 1. Clone this repository (or copy the code)
git clone https://github.com/datagutt/moblink-rust-relay.git
cd moblink-rust-relay

# Set nightly (optional)
rustup override set nightly

# 2. Build the project
cargo build --release

# 3. Run the relay
./target/release/moblink-rust-relay \
  --name "RelayName" \
  --id "UUID" \
  --streamer-url ws://192.168.1.2:7777 \
  --password "secret123" \
  --bind-address 192.168.1.10
  --log-level debug
```

### Command-Line Arguments

| Argument         | Description                                                                  | Default       | Example                                     |
|------------------|------------------------------------------------------------------------------|---------------|---------------------------------------------|
| `--name`         | Name to identify the relay                                                    | `Relay`       | `--name CameraRelay1`                       |
| `--id`           | UUID to identify the Relay                                                    | Generated     | `--id UUID`                                 |
| `--streamer-url` | WebSocket URL to connect to the streamer                                      | _None_        | `--streamer-url wss://example.com/ws`       |
| `--password`     | Password used in the challenge–response authentication                        | _None_        | `--password mySecret`                       |
| `--log-level`    | Logging verbosity (e.g., error, warn, info, debug, trace)                    | `info`        | `--log-level debug`                         |
| `--bind-address` | Local IP address to bind for UDP socket                                      | `0.0.0.0`     | `--bind-address 192.168.1.10`  |

### Bind to multiple addresses/interface

We no longer support binding to multiple addresses.
Please start multiple instances of the relay for each interface.

## Architecture

1. **WebSocket Connection**  
   - Establishes a WebSocket to `streamer_url`.  
   - Handles “Hello” messages, calculates authentication, and sends an “Identify” message.

2. **Handling Requests**  
   - When a `startTunnel` request is received, the relay spawns a task:
     - **(relay_to_destination)**: Forwards traffic from relay → destination

3. **UDP Binding**  
   - By default, it binds a UDP socket to whatever we deem to be the main network interface.

4. **Battery/Status**  
   - For demonstration, a battery percentage callback is shown. This can be replaced with actual device stats.

## FAQ

**Q:** How do I integrate this into my own application?  
**A:** You can copy the `relay.rs` and `protocol.rs` modules and adapt them. The `main.rs` file shows a simple CLI usage example.

**Q:** Why isn't this published as a crate?
**A:** I will get around to it, I promise.

---

**License**: This project is distributed under the terms of the MIT license.

Enjoy using **Moblink Rust Relay**!
