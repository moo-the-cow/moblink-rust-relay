use std::time::Duration;

use clap::Parser;
use moblink_rust::RelayService;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Password
    #[arg(long, default_value = "1234")]
    password: String,

    /// Network interfaces to allow. Localhost is never allowed.
    #[arg(long)]
    network_interfaces_to_allow: Vec<String>,

    /// Network interfaces to ignore. Ignores localhost automatically.
    #[arg(long)]
    network_interfaces_to_ignore: Vec<String>,

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
async fn main() {
    let args = Args::parse();
    setup_logging(&args.log_level);

    let relay_service = RelayService::new(
        args.password,
        args.network_interfaces_to_allow,
        args.network_interfaces_to_ignore,
    );
    relay_service.start().await;

    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}
