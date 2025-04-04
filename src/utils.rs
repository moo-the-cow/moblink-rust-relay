use log::{error, info, warn};
use rand::distr::{Alphanumeric, SampleString};
use tokio::process::Command;

pub const MDNS_SERVICE_TYPE: &str = "_moblink._tcp.local.";

pub type AnyError = Box<dyn std::error::Error + Send + Sync>;

pub fn random_string() -> String {
    Alphanumeric.sample_string(&mut rand::rng(), 64)
}

pub async fn execute_command(executable: &str, args: &[&str]) {
    let command = format_command(executable, args);
    match Command::new(executable).args(args).status().await {
        Ok(status) => {
            if status.success() {
                info!("Command '{}' succeeded!", command);
            } else {
                warn!("Command '{}' failed with status {}", command, status);
            }
        }
        Err(error) => {
            error!("Command '{}' failed with error: {}", command, error);
        }
    }
}

pub fn format_command(executable: &str, args: &[&str]) -> String {
    format!("{} {}", executable, args.join(" "))
}
