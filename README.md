# Moblink Rust Relay

Use spare devices as extra SRTLA bonding connections. The same functionality is part of [Moblin](https://github.com/eerimoq/moblin) on iOS.

Originally inspired by the Moblink Kotlin/Android code.

## Features

- **WebSocket Connection**: Connects to Moblin via WebSocket (e.g., `wss://...`)  
- **Auth Handling**: Implements the same challenge–response authentication logic as the Android client  
- **UDP Relay**: Forwards UDP packets between the remote streamer and a local destination.  
- **mDNS**: Automatically connect to nearby Moblink devices.

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
| `--name`         | Name to identify the relay                                                   | `Relay`       | `--name CameraRelay1`                       |
| `--id`           | UUID to identify the Relay                                                   | Generated     | `--id UUID`                                 |
| `--streamer-url` | WebSocket URL to connect to the streamer                                     | _None_ (multicast DNS)        | `--streamer-url wss://example.com/ws`       |
| `--password`     | Password used in the challenge–response authentication                       | _None_        | `--password mySecret`                       |
| `--log-level`    | Logging verbosity (e.g., error, warn, info, debug, trace)                    | `info`        | `--log-level debug`                         |
| `--bind-address` | Local modem IP address to bind for UDP socket                                | `0.0.0.0`     | `--bind-address 192.168.1.10`               |
| `--status-executable` | Status executable. Print status to standard output on format {"batteryPercentage": 93} | _None_ | `--status-executable ./status.sh`   |
| `--status-file` | Status file. Contains status on format {"batteryPercentage": 93}              | _None_        | `--status-file status.json`                 |

Relay status (today only battery percentage) is sent to the streamer if `--status-executable` or `--status-file` is given and outputting a valid JSON object as seen above.

### Bind to multiple addresses/interface

We no longer support binding to multiple addresses.
Please start multiple instances of the relay for each interface.

## Architecture

1. **WebSocket Connection**  
   - Establishes a WebSocket to `streamer_url`, or if not provided, tries to find nearby Moblink streamers through multicast DNS.
   - Handles “Hello” messages, calculates authentication, and sends an “Identify” message.

2. **Handling Requests**  
   - When a `startTunnel` request is received, the relay spawns two async tasks:  
     - **(relay_to_destination)**: Forwards traffic from streamer → destination  
     - **(relay_to_streamer)**: Forwards traffic from destination → streamer  

3. **UDP Binding**  
   - By default, it binds a UDP socket to whatever we deem to be the main network interface.

## FAQ

**Q:** How do I integrate this into my own application?  
**A:** Use the moblink-rust [crate](https://crates.io/crates/moblink-rust)

---

**License**: This project is distributed under the terms of the MIT license.

Enjoy using **Moblink Rust Relay**!
