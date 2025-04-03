use std::time::Duration;

use clap::Parser;
use gethostname::gethostname;
use moblink_rust::streamer;

fn hostname() -> String {
    gethostname().to_str().unwrap_or("Moblink").to_string()
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Id
    #[arg(long, default_value_t = hostname())]
    id: String,

    /// Name
    #[arg(long, default_value_t = hostname())]
    name: String,

    /// Password
    #[arg(long, default_value = "1234")]
    password: String,

    /// Websocket server listener address. Used for mDNS-SD as well right now.
    #[arg(long)]
    websocket_server_address: String,

    /// Websocket server listener port
    #[arg(long, default_value = "7777")]
    websocket_server_port: u16,

    /// TUN IP network (CIDR notation).
    /// TUN network interfaces will be assigned IP addresses from this network.
    #[arg(long, default_value = "10.3.3.0/24")]
    tun_ip_network: String,

    /// Streaming destination address
    #[arg(long)]
    destination_address: Option<String>,

    /// Streaming destination port
    #[arg(long)]
    destination_port: Option<u16>,

    /// BELABOX mode
    #[arg(long)]
    belabox: bool,

    /// Log level
    #[arg(long, default_value = "info")]
    log_level: String,
}

fn setup_logging(log_level: &str) {
    env_logger::builder()
        .default_format()
        .format_timestamp_millis()
        .parse_filters(log_level)
        .init();
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = Args::parse();
    setup_logging(&args.log_level);

    if !args.belabox {
        if args.destination_address.is_none() {
            return Err("--destination-address is required when --belabox is not given.".into());
        }
        if args.destination_port.is_none() {
            return Err("--destination-port is required when --belabox is not given.".into());
        }
    }

    let streamer = streamer::Streamer::new(
        args.id,
        args.name,
        args.websocket_server_address,
        args.websocket_server_port,
        args.tun_ip_network,
        args.password,
        args.destination_address.unwrap_or_default(),
        args.destination_port.unwrap_or_default(),
        args.belabox,
    )?;
    streamer.start().await?;

    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}
