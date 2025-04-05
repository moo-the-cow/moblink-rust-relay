use std::net::Ipv4Addr;
use std::sync::{Arc, Weak};
use std::time::Duration;

use log::info;
use mdns_sd::{ServiceDaemon, ServiceEvent};
use network_interface::{NetworkInterface, NetworkInterfaceConfig};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::MDNS_SERVICE_TYPE;
use crate::relay::{GetStatusClosure, Relay, Status};
use crate::utils::get_first_ipv4_address;

struct ServiceRelay {
    interface_name: String,
    interface_address: Ipv4Addr,
    streamer_name: String,
    streamer_url: String,
    relay: Relay,
}

impl ServiceRelay {
    async fn new(
        interface_name: String,
        interface_address: Ipv4Addr,
        streamer_name: String,
        streamer_url: String,
        password: String,
        get_status: Option<GetStatusClosure>,
    ) -> Self {
        let relay = Relay::new();
        relay
            .setup(
                streamer_url.clone(),
                password,
                uuid::Uuid::new_v4(),
                interface_name.clone(),
                |_| {},
                get_status,
            )
            .await;
        relay.start().await;
        Self {
            interface_name,
            interface_address,
            streamer_name,
            streamer_url,
            relay,
        }
    }
}

struct Streamer {
    name: String,
    url: String,
}

struct RelayServiceInner {
    me: Weak<Mutex<Self>>,
    password: String,
    network_interfaces_to_allow: Vec<String>,
    network_interfaces_to_ignore: Vec<String>,
    get_status: Option<GetStatusClosure>,
    status: Status,
    relays: Vec<ServiceRelay>,
    network_interfaces: Vec<NetworkInterface>,
    streamers: Vec<Streamer>,
    network_interface_monitor: Option<JoinHandle<()>>,
    streamers_monitor: Option<JoinHandle<()>>,
    get_status_updater: Option<JoinHandle<()>>,
}

impl RelayServiceInner {
    fn new(
        password: String,
        network_interfaces_to_allow: Vec<String>,
        network_interfaces_to_ignore: Vec<String>,
        get_status: Option<GetStatusClosure>,
    ) -> Arc<Mutex<Self>> {
        Arc::new_cyclic(|me| {
            Mutex::new(Self {
                me: me.clone(),
                password,
                network_interfaces_to_allow,
                network_interfaces_to_ignore,
                get_status,
                status: Default::default(),
                relays: Vec::new(),
                network_interfaces: Vec::new(),
                streamers: Vec::new(),
                network_interface_monitor: None,
                streamers_monitor: None,
                get_status_updater: None,
            })
        })
    }

    async fn start(&mut self) {
        self.start_network_interfaces_monitor();
        self.start_streamers_monitor();
        self.start_get_status_updater();
    }

    async fn stop(&mut self) {
        if let Some(network_interface_monitor) = self.network_interface_monitor.take() {
            network_interface_monitor.abort();
            network_interface_monitor.await.ok();
        }
        if let Some(streamers_finder) = self.streamers_monitor.take() {
            streamers_finder.abort();
            streamers_finder.await.ok();
        }
    }

    fn start_network_interfaces_monitor(&mut self) {
        let relay_service = self.me.clone();
        self.network_interface_monitor = Some(tokio::spawn(async move {
            loop {
                let Ok(interfaces) = NetworkInterface::show() else {
                    break;
                };
                let Some(relay_service) = relay_service.upgrade() else {
                    break;
                };
                {
                    let mut relay_service = relay_service.lock().await;
                    relay_service.update_network_interfaces(interfaces);
                    relay_service.updated().await;
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }));
    }

    fn update_network_interfaces(&mut self, mut interfaces: Vec<NetworkInterface>) {
        if !self.network_interfaces_to_allow.is_empty() {
            interfaces
                .retain(|interface| self.network_interfaces_to_allow.contains(&interface.name));
        }
        interfaces.retain(|interface| !self.network_interfaces_to_ignore.contains(&interface.name));
        self.network_interfaces = interfaces;
    }

    fn start_streamers_monitor(&mut self) {
        let relay_service = self.me.clone();
        self.streamers_monitor = Some(tokio::spawn(async move {
            loop {
                let Ok(browser) = ServiceDaemon::new() else {
                    return;
                };
                let Ok(receiver) = browser.browse(MDNS_SERVICE_TYPE) else {
                    return;
                };
                while let Ok(event) = receiver.recv_async().await {
                    if let ServiceEvent::ServiceResolved(info) = event {
                        info!(
                            "mDNS-SD: Found streamer {} {:?} {}",
                            info.get_fullname(),
                            info.get_addresses(),
                            info.get_port()
                        );
                        let Some(name) = info.get_property_val_str("name") else {
                            continue;
                        };
                        let Some(address) = info.get_addresses_v4().iter().next().cloned() else {
                            continue;
                        };
                        let Some(relay_service) = relay_service.upgrade() else {
                            break;
                        };
                        {
                            let mut relay_service = relay_service.lock().await;
                            relay_service.add_streamer(name.to_string(), *address, info.get_port());
                            relay_service.updated().await;
                        }
                    }
                }
            }
        }));
    }

    fn start_get_status_updater(&mut self) {
        let relay_service = self.me.clone();
        self.get_status_updater = Some(tokio::spawn(async move {
            loop {
                let Some(relay_service) = relay_service.upgrade() else {
                    break;
                };
                relay_service.lock().await.update_status().await;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }));
    }

    async fn update_status(&mut self) {
        self.status = if let Some(get_status) = &self.get_status {
            get_status().await
        } else {
            Status::default()
        }
    }

    fn add_streamer(&mut self, name: String, address: Ipv4Addr, port: u16) {
        let url = format!("ws://{}:{}", address, port);
        self.streamers.retain(|streamer| streamer.url != url);
        self.streamers.push(Streamer { name, url });
    }

    async fn updated(&mut self) {
        let old_number_of_relays = self.relays.len();
        self.add_relays().await;
        self.remove_relays().await;
        let new_number_of_relays = self.relays.len();
        if new_number_of_relays != old_number_of_relays {
            info!("Number of relays: {}", new_number_of_relays);
        }
    }

    async fn add_relays(&mut self) {
        for interface in &self.network_interfaces {
            let Some(interface_address) = get_first_ipv4_address(interface) else {
                continue;
            };
            if interface_address.is_loopback() {
                continue;
            }
            for streamer in &self.streamers {
                if self.relay_already_added(interface_address, &streamer.url) {
                    continue;
                }
                info!(
                    "Adding relay called {} with interface address {} for streamer name {} and \
                     URL {}",
                    interface.name, interface_address, streamer.name, streamer.url
                );
                self.relays.push(
                    ServiceRelay::new(
                        interface.name.clone(),
                        interface_address,
                        streamer.name.clone(),
                        streamer.url.clone(),
                        self.password.clone(),
                        self.create_get_status_closure(),
                    )
                    .await,
                );
            }
        }
    }

    pub fn create_get_status_closure(&self) -> Option<GetStatusClosure> {
        let relay_service = self.me.clone();
        Some(Box::new(move || {
            let relay_service = relay_service.clone();
            Box::pin(async move {
                if let Some(relay_service) = relay_service.upgrade() {
                    relay_service.lock().await.status.clone()
                } else {
                    Status::default()
                }
            })
        }))
    }

    fn relay_already_added(&self, interface_address: Ipv4Addr, streamer_url: &str) -> bool {
        self.relays.iter().any(|relay| {
            relay.interface_address == interface_address && relay.streamer_url == streamer_url
        })
    }

    async fn remove_relays(&mut self) {
        let mut relays_to_keep: Vec<ServiceRelay> = Vec::new();
        let mut relays_to_remove: Vec<ServiceRelay> = Vec::new();
        for relay in self.relays.drain(..) {
            if Self::should_keep_relay(&self.network_interfaces, relay.interface_address) {
                relays_to_keep.push(relay);
            } else {
                relays_to_remove.push(relay);
            }
        }
        self.relays = relays_to_keep;
        for relay in relays_to_remove {
            info!(
                "Removing relay called {} with interface address {} for streamer name {} and URL \
                 {}",
                relay.interface_name,
                relay.interface_address,
                relay.streamer_name,
                relay.streamer_url
            );
            relay.relay.stop().await;
        }
    }

    fn should_keep_relay(
        network_interfaces: &[NetworkInterface],
        interface_address: Ipv4Addr,
    ) -> bool {
        network_interfaces
            .iter()
            .any(|interface| get_first_ipv4_address(interface) == Some(interface_address))
    }
}

pub struct RelayService {
    inner: Arc<Mutex<RelayServiceInner>>,
}

impl RelayService {
    pub fn new(
        password: String,
        network_interfaces_to_allow: Vec<String>,
        network_interfaces_to_ignore: Vec<String>,
        get_status: Option<GetStatusClosure>,
    ) -> Self {
        Self {
            inner: RelayServiceInner::new(
                password,
                network_interfaces_to_allow,
                network_interfaces_to_ignore,
                get_status,
            ),
        }
    }

    pub async fn start(&self) {
        self.inner.lock().await.start().await;
    }

    pub async fn stop(&self) {
        self.inner.lock().await.stop().await;
    }
}
