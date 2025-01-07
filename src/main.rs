mod protocol;
mod relay;

use clap::Parser;
use env_logger;
use log::info;
use std::env;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Name to identify the relay
    #[arg(short, long, default_value = "Relay")]
    name: String,

    /// Streamer URL (websocket)
    #[arg(short = 'u', long)]
    streamer_url: String,

    /// Password
    #[arg(short, long)]
    password: String,

    /// Bind addresses
    #[arg(long = "bind-address", num_args = 2..)]
    bind_addresses: Vec<String>,

    /// Log level
    #[arg(short, long, default_value = "info")]
    log_level: String,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    env::set_var("RUST_LOG", args.log_level);
    env_logger::init();

    // Wrap the Relay instance in Arc<Mutex>
    let relay = Arc::new(Mutex::new(relay::Relay::new()));

    // Call setup on the wrapped Relay
    {
        let mut relay_lock = relay.lock().await;
        relay_lock
            .setup(
                args.streamer_url,
                args.password,
                args.name,
                move |status| {
                    info!("Status updated: {}", status);
                },
                move |callback| {
                    // Simulate getting the battery percentage
                    let battery_percentage = 75; // Replace with actual battery percentage retrieval
                    callback(battery_percentage);
                },
            )
            .await;
    }

    // Start the relay
    relay::Relay::start(relay.clone()).await;

    // Keep the main thread alive
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
    }
}
