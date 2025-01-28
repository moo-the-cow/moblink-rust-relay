mod protocol;
mod relay;

use std::env;
use std::sync::Arc;

use clap::Parser;
use log::info;
use tokio::sync::Mutex;
use {env_logger, uuid};

pub fn generate_relay_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Name to identify the relay
    #[arg(short, long, default_value = "Relay")]
    name: String,

    /// Relay ID (valid UUID)
    #[arg(short, long, default_value_t = String::new())]
    id: String,

    /// Streamer URL (websocket)
    #[arg(short = 'u', long)]
    streamer_url: String,

    /// Password
    #[arg(short, long)]
    password: String,

    /// Bind address
    #[arg(short, long = "bind-address", default_value_t = String::new())]
    bind_address: String,

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
        if !args.bind_address.is_empty() {
            relay_lock.set_bind_address(args.bind_address);
        }
        // If a UUID is not provided, generate a new one
        let id = if args.id.is_empty() {
            generate_relay_id()
        } else {
            args.id
        };
        relay_lock
            .setup(
                args.streamer_url,
                args.password,
                id,
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
