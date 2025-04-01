use clap::Parser;
use moblink_rust::streamer;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Id
    #[arg(long)]
    id: String,

    /// Name
    #[arg(long)]
    name: String,

    /// Password
    #[arg(long)]
    password: String,

    /// Websocket server listener address. Used for mDNS-SD as well right now.
    #[arg(long)]
    websocket_server_address: String,

    /// Websocket server listener port
    #[arg(long)]
    websocket_server_port: u16,

    /// TUN IP network (CIDR notation).
    /// TUN network interfaces will be assigned IP addresses from this network.
    #[arg(long, default_value = "10.3.3.0/24")]
    tun_ip_network: String,

    /// Streaming destination address
    #[arg(long)]
    destination_address: String,

    /// Streaming destination port
    #[arg(long)]
    destination_port: u16,

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

    let streamer = streamer::Streamer::new(
        args.id,
        args.name,
        args.websocket_server_address,
        args.websocket_server_port,
        args.tun_ip_network,
        args.password,
        args.destination_address,
        args.destination_port,
    )?;
    streamer.lock().await.start().await?;

    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}
