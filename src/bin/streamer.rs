use std::time::Duration;

use moblink_rust::streamer;

use clap::Parser;

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

    /// Websocket server listener address. Used for mDNS as well right now.
    #[arg(long)]
    websocket_server_address: String,

    /// Websocket server listener port
    #[arg(long)]
    websocket_server_port: u16,

    /// Streaming destination address
    #[arg(long)]
    destination_address: String,

    /// Streaming destination port
    #[arg(long)]
    destination_port: u16,

    /// Tunnel via relay created executable.
    /// Called with --relay-id <id> --relay-name <name> --address <address> --port <port>.
    #[arg(long)]
    tunnel_created: Option<String>,

    /// Tunnel via relay destroyed executable.
    /// Called with --relay-id <id> --relay-name <name> --address <address> --port <port>.
    #[arg(long)]
    tunnel_destroyed: Option<String>,

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
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    setup_logging(&args.log_level);

    let streamer = streamer::Streamer::new(
        args.id,
        args.name,
        args.websocket_server_address,
        args.websocket_server_port,
        args.password,
        args.destination_address,
        args.destination_port,
    );
    streamer.lock().await.start().await?;

    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}
