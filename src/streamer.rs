use crate::protocol::{
    calculate_authentication, Authentication, Hello, Identified, Identify, MessageRequest,
    MessageRequestData, MessageResponse, MessageToRelay, MessageToStreamer, MoblinkResult, Present,
    ResponseData, StartTunnelRequest, API_VERSION,
};
use crate::MDNS_SERVICE_TYPE;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use log::{debug, error, info};
use packet::{ip, udp};
use packet::{Builder as _, Packet as _};
use rand::distr::{Alphanumeric, SampleString};
use std::collections::HashMap;
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::process::Command;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use tokio_util::bytes::Bytes;
use tokio_util::codec::Framed;
use tun::{self, AsyncDevice, TunPacketCodec};

use mdns_sd::{ServiceDaemon, ServiceInfo};

type WebSocketWriter = SplitSink<WebSocketStream<TcpStream>, Message>;
type WebSocketReader = SplitStream<WebSocketStream<TcpStream>>;

type TunWriter = SplitSink<Framed<AsyncDevice, TunPacketCodec>, Vec<u8>>;
type TunReader = SplitStream<Framed<AsyncDevice, TunPacketCodec>>;

type AnyError = Box<dyn std::error::Error + Send + Sync>;

pub type TunnelCreatedClosure = Box<
    dyn Fn(String, String, String, u16) -> Pin<Box<dyn Future<Output = ()> + Send + Sync>>
        + Send
        + Sync,
>;

pub type TunnelDestroyedClosure = Box<
    dyn Fn(String, String, String, u16) -> Pin<Box<dyn Future<Output = ()> + Send + Sync>>
        + Send
        + Sync,
>;

#[derive(Debug)]
struct PacketBuilder {
    source_address: Ipv4Addr,
    source_port: u16,
    destination_address: Ipv4Addr,
    destination_port: u16,
}

impl PacketBuilder {
    fn new(
        source_address: Ipv4Addr,
        source_port: u16,
        destination_address: Ipv4Addr,
        destination_port: u16,
    ) -> Self {
        Self {
            source_address,
            source_port,
            destination_address,
            destination_port,
        }
    }

    fn pack(&self, payload: &[u8]) -> Result<Vec<u8>, AnyError> {
        Ok(ip::v4::Builder::default()
            .source(self.source_address)?
            .destination(self.destination_address)?
            .udp()?
            .source(self.source_port)?
            .destination(self.destination_port)?
            .payload(payload)?
            .build()?)
    }
}

struct Relay {
    me: Weak<Mutex<Self>>,
    streamer: Weak<Mutex<Streamer>>,
    relay_address: SocketAddr,
    writer: Option<WebSocketWriter>,
    challenge: String,
    salt: String,
    identified: bool,
    relay_id: String,
    relay_name: String,
    relay_tunnel_port: Option<u16>,
    tun_ip_address: String,
    relay_receiver: Option<JoinHandle<()>>,
    tun_receiver: Option<JoinHandle<()>>,
    linux_networking_table: u32,
    pong_received: bool,
}

impl Relay {
    pub fn new(
        streamer: Weak<Mutex<Streamer>>,
        relay_address: SocketAddr,
        writer: WebSocketWriter,
        tun_ip_address: String,
        linux_networking_table: u32,
    ) -> Arc<Mutex<Self>> {
        Arc::new_cyclic(|me| {
            Mutex::new(Self {
                me: me.clone(),
                streamer,
                relay_address,
                writer: Some(writer),
                challenge: String::new(),
                salt: String::new(),
                identified: false,
                relay_id: "".into(),
                relay_name: "".into(),
                relay_tunnel_port: None,
                tun_ip_address,
                relay_receiver: None,
                tun_receiver: None,
                linux_networking_table,
                pong_received: true,
            })
        })
    }

    fn start(&mut self, reader: WebSocketReader) {
        self.start_receiver(reader);
        self.start_pinger();
    }

    fn start_receiver(&mut self, mut reader: WebSocketReader) {
        let relay = self.me.clone();

        tokio::spawn(async move {
            let Some(relay) = relay.upgrade() else {
                return;
            };

            relay.lock().await.start_handshake().await;

            loop {
                match tokio::time::timeout(Duration::from_secs(20), reader.next()).await {
                    Ok(Some(Ok(message))) => {
                        if let Err(error) =
                            relay.lock().await.handle_websocket_message(message).await
                        {
                            error!("Relay error: {}", error);
                            break;
                        }
                    }
                    Ok(Some(Err(error))) => {
                        info!("Websocket error {}", error);
                        break;
                    }
                    Ok(None) => {
                        info!("No more websocket messages to receive");
                        break;
                    }
                    Err(_) => {
                        info!("Websocket read timeout");
                        if relay.lock().await.writer.is_none() {
                            break;
                        }
                    }
                }
            }

            let streamer = {
                let mut relay = relay.lock().await;
                info!("Relay disconnected: {}", relay.relay_address);
                relay.tunnel_destroyed().await;
                relay.streamer.upgrade()
            };
            if let Some(streamer) = streamer {
                streamer.lock().await.remove_relay(&relay).await;
            }
        });
    }

    fn start_pinger(&mut self) {
        let relay = self.me.clone();

        tokio::spawn(async move {
            loop {
                {
                    let Some(relay) = relay.upgrade() else {
                        break;
                    };
                    let mut relay = relay.lock().await;
                    if !relay.pong_received {
                        info!("Pong not received.");
                        relay.writer = None;
                        break;
                    } else {
                        relay.pong_received = false;
                        relay.send_websocket(Message::Ping(Bytes::new())).await.ok();
                    }
                }
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        });
    }

    async fn handle_websocket_message(&mut self, message: Message) -> Result<(), AnyError> {
        match message {
            Message::Text(text) => match serde_json::from_str(&text) {
                Ok(message) => self.handle_message(message).await,
                Err(error) => {
                    Err(format!("Failed to deserialize message with error: {}", error).into())
                }
            },
            Message::Ping(data) => Ok(self.send_websocket(Message::Pong(data)).await?),
            Message::Pong(_) => {
                self.pong_received = true;
                Ok(())
            }
            _ => Err(format!("Unsupported websocket message: {:?}", message).into()),
        }
    }

    async fn handle_message(&mut self, message: MessageToStreamer) -> Result<(), AnyError> {
        match message {
            MessageToStreamer::Identify(identify) => self.handle_message_identify(identify).await,
            MessageToStreamer::Response(response) => self.handle_message_response(response).await,
        }
    }

    async fn handle_message_identify(&mut self, identify: Identify) -> Result<(), AnyError> {
        let Some(streamer) = self.streamer.upgrade() else {
            return Err("No streamer".into());
        };
        if identify.authentication
            == calculate_authentication(
                &streamer.lock().await.password,
                &self.salt,
                &self.challenge,
            )
        {
            self.identified = true;
            self.relay_id = identify.id;
            self.relay_name = identify.name;
            let identified = Identified {
                result: MoblinkResult::Ok(Present {}),
            };
            self.send(MessageToRelay::Identified(identified)).await?;
            self.start_tunnel().await
        } else {
            let identified = Identified {
                result: MoblinkResult::WrongPassword(Present {}),
            };
            self.send(MessageToRelay::Identified(identified)).await?;
            Err("Relay sent wrong password".into())
        }
    }

    async fn handle_message_response(&mut self, response: MessageResponse) -> Result<(), AnyError> {
        match response.data {
            ResponseData::StartTunnel(data) => {
                self.relay_tunnel_port = Some(data.port);
                self.tunnel_created().await?;
            }
            message => {
                info!("Ignoring message {:?}", message);
            }
        }
        Ok(())
    }

    async fn tunnel_created(&mut self) -> Result<(), AnyError> {
        let Some(relay_tunnel_port) = self.relay_tunnel_port else {
            return Ok(());
        };
        info!(
            "Tunnel created: {}:{} ({}, {})",
            self.relay_address.ip(),
            relay_tunnel_port,
            self.relay_name,
            self.relay_id
        );
        self.start_udp_networking(relay_tunnel_port).await?;
        self.call_tunnel_created(relay_tunnel_port).await;
        Ok(())
    }

    async fn tunnel_destroyed(&mut self) {
        let Some(relay_tunnel_port) = self.relay_tunnel_port.take() else {
            return;
        };
        info!(
            "Tunnel destroyed: {}:{} ({}, {})",
            self.relay_address.ip(),
            relay_tunnel_port,
            self.relay_name,
            self.relay_id
        );
        self.stop_udp_networking().await;
        self.call_tunnel_destroyed(relay_tunnel_port).await;
    }

    async fn call_tunnel_created(&self, relay_tunnel_port: u16) {
        let Some(streamer) = self.streamer.upgrade() else {
            return;
        };
        let Some(tunnel_created) = &streamer.lock().await.tunnel_created else {
            return;
        };
        tunnel_created(
            self.relay_id.clone(),
            self.relay_name.clone(),
            self.relay_address.ip().to_string(),
            relay_tunnel_port,
        )
        .await;
    }

    async fn call_tunnel_destroyed(&self, relay_tunnel_port: u16) {
        let Some(streamer) = self.streamer.upgrade() else {
            return;
        };
        let Some(tunnel_destroyed) = &streamer.lock().await.tunnel_destroyed else {
            return;
        };
        tunnel_destroyed(
            self.relay_id.clone(),
            self.relay_name.clone(),
            self.relay_address.ip().to_string(),
            relay_tunnel_port,
        )
        .await;
    }

    async fn start_udp_networking(&mut self, relay_tunnel_port: u16) -> Result<(), AnyError> {
        let (tun_writer, tun_reader) = self.create_tun_device()?;
        let relay_socket = self.create_relay_socket(relay_tunnel_port).await?;
        self.setup_os_networking().await;
        let (tun_port_writer, tun_port_reader) = channel(1);
        self.start_relay_receiver(relay_socket.clone(), tun_writer, tun_port_reader)
            .await?;
        self.start_tun_receiver(tun_reader, relay_socket, tun_port_writer)
            .await;

        Ok(())
    }

    async fn stop_udp_networking(&mut self) {
        if let Some(relay_receiver) = self.relay_receiver.take() {
            relay_receiver.abort();
            relay_receiver.await.ok();
        }
        if let Some(tun_receiver) = self.tun_receiver.take() {
            tun_receiver.abort();
            tun_receiver.await.ok();
        }
        self.teardown_os_networking().await;
    }

    async fn create_relay_socket(
        &self,
        relay_tunnel_port: u16,
    ) -> Result<Arc<UdpSocket>, AnyError> {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        let tunnel_address = format!("{}:{}", self.relay_address.ip(), relay_tunnel_port);
        socket.connect(tunnel_address).await?;
        Ok(Arc::new(socket))
    }

    fn create_tun_device(&self) -> Result<(TunWriter, TunReader), AnyError> {
        let mut config = tun::Configuration::default();
        config
            .address(&self.tun_ip_address)
            .tun_name(self.tun_device_name())
            .up();
        let device = tun::create_as_async(&config)?;
        Ok(device.into_framed().split())
    }

    #[cfg(not(target_os = "macos"))]
    fn tun_device_name(&self) -> String {
        format!("moblink-{}", self.relay_name.replace(" ", "-"))
    }

    #[cfg(target_os = "macos")]
    fn tun_device_name(&self) -> String {
        format!("utun{}", 99)
    }

    async fn setup_os_networking(&self) {
        #[cfg(target_os = "linux")]
        self.setup_linux_networking().await;
    }

    #[allow(dead_code)]
    async fn setup_linux_networking(&self) {
        let Some(streamer) = self.streamer.upgrade() else {
            return;
        };
        let destination_address = &streamer.lock().await.destination_address;
        let table = self.get_linux_networking_table();
        self.teardown_linux_networking().await;
        execute_command(
            "ip",
            &[
                "route",
                "add",
                destination_address,
                "dev",
                &self.tun_device_name(),
                "proto",
                "kernel",
                "scope",
                "link",
                "src",
                &self.tun_ip_address,
                "table",
                &table,
            ],
        )
        .await;
        execute_command(
            "ip",
            &[
                "route",
                "add",
                "default",
                "via",
                &self.tun_ip_address,
                "dev",
                &self.tun_device_name(),
                "table",
                &table,
            ],
        )
        .await;
        execute_command(
            "ip",
            &[
                "rule",
                "add",
                "from",
                &self.tun_ip_address,
                "lookup",
                &table,
            ],
        )
        .await;
    }

    async fn teardown_os_networking(&self) {
        #[cfg(target_os = "linux")]
        self.teardown_linux_networking().await;
    }

    #[allow(dead_code)]
    async fn teardown_linux_networking(&self) {
        let table = self.get_linux_networking_table();
        execute_command("ip", &["rule", "del", "lookup", &table]).await;
        execute_command("ip", &["route", "flush", "table", &table]).await;
    }

    fn get_linux_networking_table(&self) -> String {
        format!("{}", self.linux_networking_table)
    }

    async fn start_tun_receiver(
        &mut self,
        mut tun_reader: TunReader,
        relay_socket: Arc<UdpSocket>,
        tun_port_writer: Sender<u16>,
    ) {
        let Some(streamer) = self.streamer.upgrade() else {
            return;
        };
        let streamer = streamer.lock().await;
        let Ok(destination_address) = Ipv4Addr::from_str(&streamer.destination_address) else {
            return;
        };
        self.tun_receiver = Some(tokio::spawn(async move {
            let mut tun_port_writer = Some(tun_port_writer);
            while let Some(packet) = tun_reader.next().await {
                if let Err(error) = Self::handle_tun_packet(
                    packet,
                    &relay_socket,
                    destination_address,
                    &mut tun_port_writer,
                )
                .await
                {
                    error!("TUN receiver: {}", error);
                    break;
                }
            }
        }));
    }

    async fn handle_tun_packet(
        packet: Result<Vec<u8>, std::io::Error>,
        relay_socket: &Arc<UdpSocket>,
        destination_address: Ipv4Addr,
        tun_port_writer: &mut Option<Sender<u16>>,
    ) -> Result<(), AnyError> {
        match packet {
            Ok(packet) => match ip::Packet::new(packet) {
                Ok(ip::Packet::V4(packet)) => {
                    debug!("TUN receiver: Got IPv4 packet: {:?}", packet);
                    if packet.protocol() == ip::Protocol::Udp
                        && packet.destination() == destination_address
                    {
                        Self::handle_tun_udp_packet(
                            packet.payload(),
                            relay_socket,
                            tun_port_writer,
                        )
                        .await?;
                    }
                }
                Ok(ip::Packet::V6(_)) => {
                    debug!("TUN receiver: Discarding IPv6 packet");
                }
                Err(error) => {
                    return Err(format!("Invalid IP packet: {}", error).into());
                }
            },
            Err(error) => {
                return Err(format!("TUN receiver: Read failed with: {}", error).into());
            }
        }
        Ok(())
    }

    async fn handle_tun_udp_packet(
        packet: &[u8],
        relay_socket: &Arc<UdpSocket>,
        tun_port_writer: &mut Option<Sender<u16>>,
    ) -> Result<(), AnyError> {
        match udp::Packet::new(packet) {
            Ok(packet) => {
                debug!("TUN receiver: Got UDP packet: {:?}", packet);
                if let Some(tun_port_writer) = tun_port_writer.take() {
                    tun_port_writer.send(packet.source()).await.ok();
                }
                if let Err(error) = relay_socket.send(packet.payload()).await {
                    return Err(format!("Send error {}", error).into());
                }
            }
            Err(error) => {
                return Err(format!("Invalid UDP packet: {}", error).into());
            }
        }
        Ok(())
    }

    async fn start_relay_receiver(
        &mut self,
        relay_socket: Arc<UdpSocket>,
        mut tun_writer: TunWriter,
        mut tun_port_reader: Receiver<u16>,
    ) -> Result<(), AnyError> {
        let Some(streamer) = self.streamer.upgrade() else {
            return Err("No streamer".into());
        };
        let streamer = streamer.lock().await;
        let destination_address = streamer.destination_address.clone();
        let destination_port = streamer.destination_port;
        let tun_ip_address = self.tun_ip_address.clone();

        self.relay_receiver = Some(tokio::spawn(async move {
            let Ok(destination_address) = Ipv4Addr::from_str(&destination_address) else {
                return;
            };
            let Ok(tun_ip_address) = Ipv4Addr::from_str(&tun_ip_address) else {
                return;
            };
            info!("Relay receiver: Waiting for TUN port");
            let Some(tun_port) = tun_port_reader.recv().await else {
                return;
            };
            let packet_builder = PacketBuilder::new(
                destination_address,
                destination_port,
                tun_ip_address,
                tun_port,
            );
            info!("Relay receiver: Ready with {:?}", packet_builder);
            let mut buffer = vec![0; 2048];
            loop {
                match relay_socket.recv(&mut buffer).await {
                    Ok(length) => {
                        debug!("Relay receiver: Got {:?}", &buffer[..length]);
                        let Ok(packet) = packet_builder.pack(&buffer[..length]) else {
                            error!("Relay receiver: IP create error");
                            break;
                        };
                        if let Err(error) = tun_writer.send(packet).await {
                            error!("Relay receiver: Send error {}", error);
                            break;
                        }
                    }
                    Err(error) => {
                        error!("Relay receiver: Error {}", error);
                        break;
                    }
                }
            }
        }));
        Ok(())
    }

    async fn start_handshake(&mut self) {
        self.challenge = random_string();
        self.salt = random_string();
        self.send_hello().await;
        self.identified = false;
    }

    async fn start_tunnel(&mut self) -> Result<(), AnyError> {
        let Some(streamer) = self.streamer.upgrade() else {
            return Err("No streamer".into());
        };
        let streamer = streamer.lock().await;
        let start_tunnel = StartTunnelRequest {
            address: streamer.destination_address.clone(),
            port: streamer.destination_port,
        };
        let request = MessageRequest {
            id: 1,
            data: MessageRequestData::StartTunnel(start_tunnel),
        };
        self.send(MessageToRelay::Request(request)).await
    }

    async fn send_hello(&mut self) {
        let hello = MessageToRelay::Hello(Hello {
            api_version: API_VERSION.into(),
            authentication: Authentication {
                challenge: self.challenge.clone(),
                salt: self.salt.clone(),
            },
        });
        self.send(hello).await.ok();
    }

    async fn send(&mut self, message: MessageToRelay) -> Result<(), AnyError> {
        let text = serde_json::to_string(&message)?;
        self.send_websocket(Message::Text(text.into())).await
    }

    async fn send_websocket(&mut self, message: Message) -> Result<(), AnyError> {
        if let Some(writer) = self.writer.as_mut() {
            writer.send(message).await?;
        } else {
            return Err("No websocket writer".into());
        }
        Ok(())
    }
}

pub struct Streamer {
    me: Weak<Mutex<Self>>,
    id: String,
    name: String,
    address: String,
    port: u16,
    password: String,
    destination_address: String,
    destination_port: u16,
    tunnel_created: Option<TunnelCreatedClosure>,
    tunnel_destroyed: Option<TunnelDestroyedClosure>,
    relays: Vec<Arc<Mutex<Relay>>>,
    linux_networking_table_offset: u32,
    tun_ip_addresses: Vec<String>,
}

impl Streamer {
    pub fn new(
        id: String,
        name: String,
        address: String,
        port: u16,
        password: String,
        destination_address: String,
        destination_port: u16,
        tunnel_created: Option<TunnelCreatedClosure>,
        tunnel_destroyed: Option<TunnelDestroyedClosure>,
    ) -> Arc<Mutex<Self>> {
        Arc::new_cyclic(|me| {
            Mutex::new(Self {
                me: me.clone(),
                id,
                name,
                address,
                port,
                password,
                destination_address,
                destination_port,
                tunnel_created,
                tunnel_destroyed,
                relays: Vec::new(),
                linux_networking_table_offset: 0,
                tun_ip_addresses: (1..20).map(|i| format!("10.0.0.{}", i)).collect(),
            })
        })
    }

    pub async fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let listener_address = format!("{}:{}", self.address, self.port);
        let listener = TcpListener::bind(&listener_address).await?;
        info!("WebSocket server listening on '{}'", listener_address);
        let (service_daemon, service_info) = self.create_mdns_service()?;
        let streamer = self.me.clone();

        tokio::spawn(async move {
            if let Err(error) = service_daemon.register(service_info) {
                error!("Failed to register mDNS service with error: {}", error);
            }

            while let Ok((tcp_stream, relay_address)) = listener.accept().await {
                if let Some(streamer) = streamer.upgrade() {
                    streamer
                        .lock()
                        .await
                        .handle_relay_connection(tcp_stream, relay_address)
                        .await;
                } else {
                    break;
                }
            }
        });

        Ok(())
    }

    fn create_mdns_service(
        &self,
    ) -> Result<(ServiceDaemon, ServiceInfo), Box<dyn std::error::Error>> {
        let service_daemon = ServiceDaemon::new()?;
        let properties = HashMap::from([("name".to_string(), self.name.clone())]);
        let service_info = ServiceInfo::new(
            MDNS_SERVICE_TYPE,
            &self.id,
            &format!("{}.local.", self.id),
            self.make_mdns_address(),
            self.port,
            properties,
        )?;
        Ok((service_daemon, service_info))
    }

    fn make_mdns_address(&self) -> String {
        if self.address == "0.0.0.0" {
            "".into()
        } else {
            self.address.clone()
        }
    }

    async fn handle_relay_connection(&mut self, tcp_stream: TcpStream, relay_address: SocketAddr) {
        match tokio_tungstenite::accept_async(tcp_stream).await {
            Ok(websocket_stream) => {
                info!("Relay connected: {}", relay_address);
                let (writer, reader) = websocket_stream.split();
                let Some(tun_ip_address) = self.tun_ip_addresses.pop() else {
                    return;
                };
                let relay = Relay::new(
                    self.me.clone(),
                    relay_address,
                    writer,
                    tun_ip_address,
                    self.get_linux_networking_table(),
                );
                relay.lock().await.start(reader);
                self.add_relay(relay);
            }
            Err(error) => {
                error!("Relay websocket handshake failed with: {}", error);
            }
        }
    }

    fn get_linux_networking_table(&mut self) -> u32 {
        self.linux_networking_table_offset += 1;
        self.linux_networking_table_offset %= 4800;
        300 + self.linux_networking_table_offset
    }

    fn add_relay(&mut self, relay: Arc<Mutex<Relay>>) {
        self.relays.push(relay);
        self.log_number_of_relays();
    }

    async fn remove_relay(&mut self, relay: &Arc<Mutex<Relay>>) {
        let tun_ip_address = relay.lock().await.tun_ip_address.clone();
        self.tun_ip_addresses.insert(0, tun_ip_address);
        self.relays.retain(|r| !Arc::ptr_eq(r, relay));
        self.log_number_of_relays();
    }

    fn log_number_of_relays(&self) {
        info!("Number of relays: {}", self.relays.len())
    }
}

fn random_string() -> String {
    Alphanumeric.sample_string(&mut rand::rng(), 64)
}

async fn execute_command(executable: &str, args: &[&str]) {
    Command::new(executable).args(args).status().await.ok();
}
