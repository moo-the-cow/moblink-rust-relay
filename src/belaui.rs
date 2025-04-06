use std::collections::HashMap;

use serde::Deserialize;
use tokio::fs::File;
use tokio::io::AsyncReadExt;

use crate::utils::AnyError;

pub const CONFIG_JSON_PATH: &str = "/opt/belaUI/config.json";
const RELAYS_CACHE_JSON_PATH: &str = "/opt/belaUI/relays_cache.json";

#[derive(Deserialize)]
struct ConfigJson {
    srtla_addr: Option<String>,
    srtla_port: Option<u16>,
    relay_server: Option<String>,
}

#[derive(Deserialize)]
struct RelaysCacheRelay {
    addr: String,
    port: u16,
}

#[derive(Deserialize)]
struct RelaysCacheJson {
    servers: HashMap<String, RelaysCacheRelay>,
}

pub struct Config {
    address: String,
    port: u16,
}

impl Config {
    pub async fn new_from_file() -> Result<Self, AnyError> {
        let mut file = File::open(CONFIG_JSON_PATH).await?;
        let mut contents = vec![];
        file.read_to_end(&mut contents).await?;
        let contents = String::from_utf8(contents)?;
        let config: ConfigJson = serde_json::from_str(&contents)?;
        let (address, port) = if Self::is_manual_configuration(&config) {
            Self::get_manual_configuration(&config).await?
        } else {
            Self::get_belabox_cloud_configuration(&config).await?
        };
        Ok(Self { address, port })
    }

    pub fn get_address(&self) -> String {
        self.address.clone()
    }

    pub fn get_port(&self) -> u16 {
        self.port
    }

    fn is_manual_configuration(config: &ConfigJson) -> bool {
        config.srtla_addr.is_some()
    }

    async fn get_manual_configuration(config: &ConfigJson) -> Result<(String, u16), AnyError> {
        let Some(srtla_addr) = &config.srtla_addr else {
            return Err("SRTLA address missing".into());
        };
        let Some(srtla_port) = config.srtla_port else {
            return Err("SRTLA port missing".into());
        };
        Ok((srtla_addr.clone(), srtla_port))
    }

    async fn get_belabox_cloud_configuration(
        config: &ConfigJson,
    ) -> Result<(String, u16), AnyError> {
        let Some(relay_server) = &config.relay_server else {
            return Err("Relay server missing".into());
        };
        let mut file = File::open(RELAYS_CACHE_JSON_PATH).await?;
        let mut contents = vec![];
        file.read_to_end(&mut contents).await?;
        let contents = String::from_utf8(contents)?;
        let relays_cache: RelaysCacheJson = serde_json::from_str(&contents)?;
        let Some(relay) = relays_cache.servers.get(relay_server) else {
            return Err("Relay server entry mising".into());
        };
        Ok((relay.addr.clone(), relay.port))
    }
}
