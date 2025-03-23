mod protocol;
mod relay;

use std::io::Write;
use std::time::Duration;
use tokio::{fs::File, io::AsyncReadExt, process::Command};

use clap::Parser;
use log::{debug, error, info, warn};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use relay::GetStatusClosure;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Name to identify the relay
    #[arg(short, long, default_value = "Relay")]
    name: String,

    /// Relay ID (valid UUID)
    #[arg(short, long)]
    id: Option<String>,

    /// Streamer URL (websocket) - optional if using mDNS
    #[arg(short = 'u', long)]
    streamer_url: Option<String>,

    /// Password
    #[arg(short, long)]
    password: String,

    /// Bind address
    #[arg(short, long = "bind-address", default_value_t = String::new())]
    bind_address: String,

    /// Log level
    #[arg(short, long, default_value = "info")]
    log_level: String,

    /// Status executable.
    /// Print status to standard output on format {"batteryPercentage": 93}.
    #[arg(long)]
    status_executable: Option<String>,

    /// Status file.
    /// Contains status on format {"batteryPercentage": 93}.
    #[arg(long)]
    status_file: Option<String>,
}

fn setup_logging(log_level: Option<String>) {
    let mut log_cfg = env_logger::builder();
    log_cfg.format(|buf, record| {
        let ts = buf.timestamp_micros();
        let style = buf.default_level_style(record.level());
        writeln!(
            buf,
            "[{ts} {:?} {} {style}{}{style:#}] {}",
            std::thread::current().id(),
            record.target(),
            record.level(),
            record.args()
        )
    });
    if let Some(log_filters) = &log_level {
        log_cfg.parse_filters(log_filters);
    }
    log_cfg.init();
}

fn create_get_status_closure(
    status_executable: &Option<String>,
    status_file: &Option<String>,
) -> GetStatusClosure {
    let status_executable = status_executable.clone();
    let status_file = status_file.clone();
    Box::new(move || {
        let status_executable = status_executable.clone();
        let status_file = status_file.clone();
        Box::pin(async move {
            let output = if let Some(status_executable) = &status_executable {
                let Ok(output) = Command::new(status_executable).output().await else {
                    return Default::default();
                };
                output.stdout
            } else if let Some(status_file) = &status_file {
                let Ok(mut file) = File::open(status_file).await else {
                    return Default::default();
                };
                let mut contents = vec![];
                if file.read_to_end(&mut contents).await.is_err() {
                    return Default::default();
                }
                contents
            } else {
                return Default::default();
            };
            let Ok(output) = String::from_utf8(output) else {
                return Default::default();
            };
            match serde_json::from_str(&output) {
                Ok(status) => status,
                Err(e) => {
                    error!("Failed to decode status with error: {e}");
                    Default::default()
                }
            }
        })
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    setup_logging(Some(args.log_level.clone()));

    // Get or generate relay ID
    let relay_id = args.id.unwrap_or(uuid::Uuid::new_v4().to_string());
    let relay_id_clone = relay_id.clone();

    // mDNS discovery task
    let password = args.password.clone();
    let name = args.name.clone();

    let streamer_url_clone = args.streamer_url.clone();
    let bind_address_clone = args.bind_address.clone();

    let status_executable = args.status_executable.clone();
    let status_file = args.status_file.clone();

    let mdns_task = tokio::spawn(async move {
        let mut retries = 0;

        if streamer_url_clone.is_some() {
            info!("Using provided streamer URL, skipping mDNS discovery");
            return;
        }

        let relay = relay::Relay::new();

        loop {
            let mdns = ServiceDaemon::new().expect("Failed to create mDNS daemon");
            let service_type = "_moblink._tcp.local.";
            let receiver = mdns
                .browse(service_type)
                .expect("Failed to browse services");

            info!("Searching for Moblink streamers via mDNS...");

            while let Ok(event) = receiver.recv_async().await {
                let mut relay = relay.lock().await;
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        if relay.is_started() {
                            warn!("Relay already started, skipping discovery");
                            continue;
                        }
                        // Handle network interface binding
                        if !bind_address_clone.is_empty() {
                            debug!("Binding to network interface: {}", bind_address_clone);
                            relay.set_bind_address(bind_address_clone.clone());
                        }

                        let port = info.get_port();
                        for ip in info.get_addresses() {
                            if ip.is_loopback() || ip.is_multicast() {
                                continue;
                            }

                            // Skip IPv6 for now
                            if ip.is_ipv6() {
                                continue;
                            }

                            let streamer_url = format!("ws://{}:{}", ip, port);
                            info!("Discovered Moblink streamer at {}", streamer_url);

                            debug!("Setting up relay...");
                            relay
                                .setup(
                                    streamer_url,
                                    password.clone(),
                                    relay_id_clone.clone(),
                                    name.clone(),
                                    |status| info!("Status: {}", status),
                                    create_get_status_closure(&status_executable, &status_file),
                                )
                                .await;
                        }
                        debug!("Starting relay...");
                        relay.start().await;
                    }
                    ServiceEvent::ServiceRemoved(_, _) => {
                        warn!("Streamer service removed");
                        relay.stop().await;
                    }
                    _ => {}
                }
            }

            // Reconnect logic with backoff
            let delay = Duration::from_secs(2u64.pow(retries));
            warn!("No streamers found, retrying in {:?}...", delay);
            tokio::time::sleep(delay).await;
            retries = (retries + 1).min(5);
        }
    });

    if let Err(e) = mdns_task.await {
        warn!("mDNS task failed: {:?}", e);
    }

    // If URL was provided, use it directly
    let relay = relay::Relay::new();

    if let Some(streamer_url) = args.streamer_url {
        let mut relay = relay.lock().await;
        // Handle network interface binding
        if !args.bind_address.is_empty() {
            relay.set_bind_address(args.bind_address);
        }
        relay
            .setup(
                streamer_url,
                args.password,
                relay_id,
                args.name,
                |status| info!("Status: {}", status),
                create_get_status_closure(&args.status_executable, &args.status_file),
            )
            .await;
        relay.start().await;
    }

    // Keep main alive
    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}
