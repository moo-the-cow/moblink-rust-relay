use std::net::Ipv4Addr;
use std::time::Duration;

use log::{error, info, warn};
use network_interface::{Addr, NetworkInterface};
use rand::distr::{Alphanumeric, SampleString};
use tokio::net::lookup_host;
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

pub async fn resolve_host(address: &str) -> Result<String, AnyError> {
    for _ in 0..50 {
        match lookup_host(format!("{}:9999", address)).await {
            Ok(mut addresses) => {
                if let Some(address) = addresses.next() {
                    return Ok(address.ip().to_string());
                } else {
                    warn!("No address found for {}", address);
                }
            }
            Err(error) => {
                warn!("DNS lookup for '{}' failed with error {}", address, error);
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err("DNS lookup failed after timeout".into())
}

pub fn get_first_ipv4_address(interface: &NetworkInterface) -> Option<Ipv4Addr> {
    for address in &interface.addr {
        if let Addr::V4(address) = address {
            return Some(address.ip);
        }
    }
    None
}
