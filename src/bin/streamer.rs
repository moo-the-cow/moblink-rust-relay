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

    /// Websocket server listener address.
    #[arg(long, default_value = "0.0.0.0")]
    websocket_server_address: String,

    /// Websocket server listener port
    #[arg(long, default_value = "7777")]
    websocket_server_port: u16,

    /// TUN IP network (CIDR notation).
    #[arg(long, default_value = "10.3.3.0/24")]
    tun_ip_network: String,

    /// Destination address (e.g., RIST receiver IP)
    #[arg(long)]
    destination_address: String,

    /// Destination port
    #[arg(long)]
    destination_port: u16,

    /// Log level
    #[arg(long, default_value = "info")]
    log_level: String,

    /// No log timestamps
    #[arg(long)]
    no_log_timestamps: bool,
}

fn setup_logging(timestamps: bool, log_level: &str) {
    let mut builder = env_logger::builder();
    if timestamps {
        builder.format_timestamp_millis()
    } else {
        builder.format_timestamp(None)
    }
    .parse_filters(log_level)
    .init();
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = Args::parse();
    setup_logging(!args.no_log_timestamps, &args.log_level);

    let streamer = streamer::Streamer::new(streamer::StreamerConfig {
        id: args.id,
        name: args.name,
        address: args.websocket_server_address,
        port: args.websocket_server_port,
        tun_ip_network: args.tun_ip_network,
        password: args.password,
        destination_address: args.destination_address,
        destination_port: args.destination_port,
    })?;
    streamer.start().await?;

    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}
