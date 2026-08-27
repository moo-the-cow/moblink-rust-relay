use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::str::FromStr;
use std::sync::{Arc, Weak};
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use ipnetwork::Ipv4Network;
use log::{debug, error, info, warn};
use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};
use packet::{Builder as _, Packet, ip, udp};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::select;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::task::JoinHandle;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::bytes::Bytes;
use tokio_util::codec::Framed;
use tun::{self, AsyncDevice, TunPacketCodec};
use uuid::Uuid;

use crate::protocol::{
    API_VERSION, Authentication, Hello, Identified, Identify, MessageRequest, MessageRequestData,
    MessageResponse, MessageToRelay, MessageToStreamer, MoblinkResult, Present, ResponseData,
    StartTunnelRequest, calculate_authentication,
};
use crate::utils::{AnyError, random_string};
use crate::MDNS_SERVICE_TYPE;

type WebSocketWriter = SplitSink<WebSocketStream<TcpStream>, Message>;
type WebSocketReader = SplitStream<WebSocketStream<TcpStream>>;

type TunWriter = SplitSink<Framed<AsyncDevice, TunPacketCodec>, Vec<u8>>;
type TunReader = SplitStream<Framed<AsyncDevice, TunPacketCodec>>;

#[derive(Debug, Clone)]
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
    streamer: Weak<Mutex<StreamerInner>>,
    relay_address: SocketAddr,
    writer: Option<WebSocketWriter>,
    challenge: String,
    salt: String,
    identified: bool,
    relay_id: Uuid,
    relay_name: String,
    relay_tunnel_port: Option<u16>,
    tun_ip_address: String,
    tun_writer: Option<TunWriter>,
    tun_reader: Option<TunReader>,
    relay_receiver: Option<JoinHandle<()>>,
    tun_receiver: Option<JoinHandle<()>>,
    unique_index: u32,
    pong_received: bool,
    tun_device_name: String,
    connected: bool,
    destination_address: String,
    destination_port: u16,
}

impl Relay {
    pub fn new(
        streamer: Weak<Mutex<StreamerInner>>,
        relay_address: SocketAddr,
        writer: WebSocketWriter,
        tun_ip_address: String,
        unique_index: u32,
        destination_address: String,
        destination_port: u16,
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
                relay_id: Uuid::new_v4(),
                relay_name: "".into(),
                relay_tunnel_port: None,
                tun_ip_address,
                tun_writer: None,
                tun_reader: None,
                relay_receiver: None,
                tun_receiver: None,
                unique_index,
                pong_received: true,
                tun_device_name: String::new(),
                connected: false,
                destination_address,
                destination_port,
            })
        })
    }

    fn start(&mut self, reader: WebSocketReader) {
        self.connected = true;
        self.start_websocket_receiver(reader);
        self.start_pinger();
    }

    fn start_websocket_receiver(&mut self, mut reader: WebSocketReader) {
        let relay_weak = self.me.clone();

        tokio::spawn(async move {
            let Some(relay_arc) = relay_weak.upgrade() else {
                return;
            };

            relay_arc.lock().await.start_handshake().await;

            loop {
                if !relay_arc.lock().await.connected {
                    break;
                }

                match tokio::time::timeout(Duration::from_secs(60), reader.next()).await {
                    Ok(Some(Ok(message))) => {
                        if let Err(error) =
                            relay_arc.lock().await.handle_websocket_message(message).await
                        {
                            error!("Relay error: {}", error);
                            break;
                        }
                    }
                    Ok(Some(Err(error))) => {
                        info!("Websocket error: {}", error);
                        break;
                    }
                    Ok(None) => {
                        info!("No more websocket messages to receive");
                        break;
                    }
                    Err(_) => {
                        info!("Websocket read timeout");
                        if relay_arc.lock().await.writer.is_none() {
                            break;
                        }
                    }
                }
            }

            let mut relay_guard = relay_arc.lock().await;
            info!("Relay disconnected: {}", relay_guard.relay_address);
            relay_guard.connected = false;
            relay_guard.cleanup().await;
            let streamer = relay_guard.streamer.upgrade();
            drop(relay_guard);
            if let Some(streamer) = streamer {
                streamer.lock().await.remove_relay(&relay_arc).await;
            }
        });
    }

    fn start_pinger(&mut self) {
        let relay_weak = self.me.clone();

        tokio::spawn(async move {
            loop {
                {
                    let relay_arc = relay_weak.upgrade();
                    if relay_arc.is_none() {
                        break;
                    }
                    let relay_arc = relay_arc.unwrap();
                    let mut relay = relay_arc.lock().await;
                    if !relay.connected {
                        break;
                    }
                    if !relay.pong_received {
                        info!("Pong not received, disconnecting");
                        relay.connected = false;
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

    async fn cleanup(&mut self) {
        info!("Cleaning up relay: {} (TUN: {})", self.relay_name, self.tun_device_name);

        if let Some(tun_receiver) = self.tun_receiver.take() {
            tun_receiver.abort();
            let _ = tun_receiver.await;
        }

        if let Some(relay_receiver) = self.relay_receiver.take() {
            relay_receiver.abort();
            let _ = relay_receiver.await;
        }

        if let Some(tun_writer) = self.tun_writer.take() {
            drop(tun_writer);
        }
        if let Some(tun_reader) = self.tun_reader.take() {
            drop(tun_reader);
        }

        if let Some(mut writer) = self.writer.take() {
            let _ = writer.close().await;
        }

        self.identified = false;
        self.relay_tunnel_port = None;
        self.pong_received = false;

        info!("Relay cleanup complete: {}", self.relay_name);
    }

    async fn handle_websocket_message(&mut self, message: Message) -> Result<(), AnyError> {
        debug!("Websocket got: {:?}", message);
        match message {
            Message::Text(text) => match serde_json::from_str(&text) {
                Ok(message) => self.handle_message(message).await,
                Err(error) => {
                    warn!("Failed to deserialize message: {}", error);
                    Err(format!("Failed to deserialize message with error: {}", error).into())
                }
            },
            Message::Ping(data) => {
                info!("Received ping, sending pong");
                Ok(self.send_websocket(Message::Pong(data)).await?)
            }
            Message::Pong(_) => {
                self.pong_received = true;
                Ok(())
            }
            Message::Close(_) => {
                info!("Received close message from relay");
                self.connected = false;
                Ok(())
            }
            _ => Err(format!("Unsupported websocket message: {:?}", message).into()),
        }
    }

    async fn handle_message(&mut self, message: MessageToStreamer) -> Result<(), AnyError> {
        match message {
            MessageToStreamer::Identify(identify) => {
                info!("Received Identify message from relay");
                self.handle_message_identify(identify).await
            }
            MessageToStreamer::Response(response) => {
                info!("Received Response message");
                self.handle_message_response(response).await
            }
        }
    }

    async fn handle_message_identify(&mut self, identify: Identify) -> Result<(), AnyError> {
        let Some(streamer) = self.streamer.upgrade() else {
            return Err("No streamer".into());
        };
        let streamer = streamer.lock().await;

        info!("Relay identifying with ID: {}, Name: {}", identify.id, identify.name);

        if identify.authentication
            == calculate_authentication(&streamer.password, &self.salt, &self.challenge)
        {
            self.identified = true;
            self.relay_id = identify.id;
            self.relay_name = identify.name;
            self.tun_device_name = format!("mob{}-{}", self.unique_index,
                self.relay_name.replace(|c: char| !c.is_ascii() || c.is_whitespace(), "-"));
            info!("Relay identified: {} ({})", self.relay_name, self.relay_id);

            let identified = Identified {
                result: MoblinkResult::Ok(Present {}),
            };
            info!("Sending Identified OK response");
            self.send(MessageToRelay::Identified(identified)).await?;

            info!("Creating TUN device for RIST to use...");
            self.create_tun_device()?;

            info!("TUN device {} with IP {} is ready for RIST (miface={})",
                self.tun_device_name, self.tun_ip_address, self.tun_device_name);

            info!("Starting tunnel to {}:{}", self.destination_address, self.destination_port);
            self.start_tunnel().await?;

            Ok(())
        } else {
            warn!("Relay sent wrong password");
            let identified = Identified {
                result: MoblinkResult::WrongPassword(Present {}),
            };
            self.send(MessageToRelay::Identified(identified)).await?;
            Err("Relay sent wrong password".into())
        }
    }

    async fn handle_message_response(&mut self, response: MessageResponse) -> Result<(), AnyError> {
        info!("Handling response: id={}, result={:?}", response.id, response.result);
        match response.result {
            MoblinkResult::Ok(_) => {
                match response.data {
                    ResponseData::StartTunnel(data) => {
                        info!("Received StartTunnel response with port: {}", data.port);
                        self.relay_tunnel_port = Some(data.port);
                        self.tunnel_created().await?;
                    }
                    _ => {
                        info!("Ignoring response data: {:?}", response.data);
                    }
                }
            }
            MoblinkResult::WrongPassword(_) => {
                error!("Wrong password response from relay");
                return Err("Wrong password".into());
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
        Ok(())
    }

    async fn start_udp_networking(&mut self, relay_tunnel_port: u16) -> Result<(), AnyError> {
        let relay_socket = self.create_relay_socket(relay_tunnel_port).await?;
        let (tun_port_writer, tun_port_reader) = channel(1);

        let tun_writer = self.tun_writer.take().expect("TUN writer not available");
        let tun_reader = self.tun_reader.take().expect("TUN reader not available");

        self.start_relay_receiver(relay_socket.clone(), tun_writer, tun_port_reader)
            .await?;
        self.start_tun_forwarder(tun_reader, relay_socket, tun_port_writer)
            .await;

        Ok(())
    }

    async fn create_relay_socket(
        &self,
        relay_tunnel_port: u16,
    ) -> Result<Arc<UdpSocket>, AnyError> {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        let tunnel_address = format!("{}:{}", self.relay_address.ip(), relay_tunnel_port);
        info!("Connecting to relay tunnel at: {}", tunnel_address);
        socket.connect(tunnel_address).await?;
        Ok(Arc::new(socket))
    }

    fn create_tun_device(&mut self) -> Result<(), AnyError> {
        let mut config = tun::Configuration::default();
        config
            .address(&self.tun_ip_address)
            .tun_name(&self.tun_device_name)
            .up();
        let device = tun::create_as_async(&config)?;
        info!("Created TUN device: {} with IP {}", self.tun_device_name, self.tun_ip_address);

        let (writer, reader) = device.into_framed().split();
        self.tun_writer = Some(writer);
        self.tun_reader = Some(reader);

        Ok(())
    }

    async fn start_tun_forwarder(
        &mut self,
        mut tun_reader: TunReader,
        relay_socket: Arc<UdpSocket>,
        tun_port_writer: Sender<u16>,
    ) {
        let destination_address = self.destination_address.clone();

        self.tun_receiver = Some(tokio::spawn(async move {
            let mut tun_port: u16 = 0u16;

            while let Some(packet) = tun_reader.next().await {
                match packet {
                    Ok(data) => {
                        if let Ok(ip_packet) = ip::Packet::new(data) {
                            if let ip::Packet::V4(ip_packet) = ip_packet {
                                if ip_packet.protocol() == ip::Protocol::Udp {
                                    let dest_ip = ip_packet.destination();
                                    if let Ok(dest_addr) = Ipv4Addr::from_str(&destination_address) {
                                        if dest_ip == dest_addr {
                                            if let Ok(udp_packet) = udp::Packet::new(ip_packet.payload()) {
                                                let payload = udp_packet.payload().to_vec();

                                                let new_tun_port = udp_packet.source();
                                                if new_tun_port != tun_port {
                                                    debug!("TUN port changed: {} -> {}", tun_port, new_tun_port);
                                                    let _ = tun_port_writer.send(new_tun_port).await;
                                                    tun_port = new_tun_port;
                                                }

                                                if let Err(error) = relay_socket.send(&payload).await {
                                                    error!("Failed to forward packet: {}", error);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("TUN read error: {}", e);
                        break;
                    }
                }
            }
        }));
    }

    async fn start_tunnel(&mut self) -> Result<(), AnyError> {
        if !self.identified {
            info!("Not identified yet, waiting...");
            return Ok(());
        }

        info!("Starting tunnel to configured destination {}:{}", self.destination_address, self.destination_port);
        let start_tunnel = StartTunnelRequest {
            address: self.destination_address.clone(),
            port: self.destination_port,
        };
        let request = MessageRequest {
            id: 1,
            data: MessageRequestData::StartTunnel(start_tunnel),
        };
        self.send(MessageToRelay::Request(request)).await
    }

    async fn start_relay_receiver(
        &mut self,
        relay_socket: Arc<UdpSocket>,
        mut tun_writer: TunWriter,
        mut tun_port_reader: Receiver<u16>,
    ) -> Result<(), AnyError> {
        let tun_ip_address = self.tun_ip_address.clone();
        let destination_address = self.destination_address.clone();
        let destination_port = self.destination_port;

        self.relay_receiver = Some(tokio::spawn(async move {
            let Ok(tun_ip_addr) = Ipv4Addr::from_str(&tun_ip_address) else {
                error!("Failed to parse TUN IP address: {}", tun_ip_address);
                return;
            };
            let Ok(dest_ip) = Ipv4Addr::from_str(&destination_address) else {
                error!("Failed to parse destination address: {}", destination_address);
                return;
            };

            let mut buffer = vec![0; 2048];
            let mut packet_builder = PacketBuilder::new(dest_ip, destination_port, tun_ip_addr, 10000);

            loop {
                if let Err(error) = select! {
                    result = relay_socket.recv(&mut buffer) => {
                        Self::handle_relay_packet(&mut tun_writer, &packet_builder, result, &buffer).await
                    }
                    tun_port = tun_port_reader.recv() => {
                        Self::handle_tun_port(&mut packet_builder, tun_port)
                    }
                } {
                    error!("Relay receiver: Error {}", error);
                    break;
                }
            }
        }));
        Ok(())
    }

    async fn handle_relay_packet(
        tun_writer: &mut TunWriter,
        packet_builder: &PacketBuilder,
        result: Result<usize, std::io::Error>,
        buffer: &[u8],
    ) -> Result<(), AnyError> {
        match result {
            Ok(length) => {
                debug!("Relay receiver: Got {} bytes", length);
                let Ok(packet) = packet_builder.pack(&buffer[..length]) else {
                    return Err("Relay receiver: IP create error".into());
                };
                if let Err(error) = tun_writer.send(packet).await {
                    Err(format!("Relay receiver: Send error {}", error).into())
                } else {
                    Ok(())
                }
            }
            Err(error) => Err(format!("Relay receiver: Error {}", error).into()),
        }
    }

    fn handle_tun_port(
        packet_builder: &mut PacketBuilder,
        tun_port: Option<u16>,
    ) -> Result<(), AnyError> {
        let Some(tun_port) = tun_port else {
            return Err("TUN port missing".into());
        };
        packet_builder.destination_port = tun_port;
        info!("Relay receiver: Ready with port {}", tun_port);
        Ok(())
    }

    async fn start_handshake(&mut self) {
        self.challenge = random_string();
        self.salt = random_string();
        info!("Starting handshake with challenge: {}", self.challenge);
        self.send_hello().await;
        self.identified = false;
    }

    async fn send_hello(&mut self) {
        let hello = MessageToRelay::Hello(Hello {
            api_version: API_VERSION.into(),
            authentication: Authentication {
                challenge: self.challenge.clone(),
                salt: self.salt.clone(),
            },
        });
        info!("Sending Hello message");
        self.send(hello).await.ok();
    }

    async fn send(&mut self, message: MessageToRelay) -> Result<(), AnyError> {
        let text = serde_json::to_string(&message)?;
        debug!("Sending message: {}", text);
        self.send_websocket(Message::Text(text.into())).await
    }

    async fn send_websocket(&mut self, message: Message) -> Result<(), AnyError> {
        match self.writer.as_mut() {
            Some(writer) => {
                debug!("Websocket sending: {:?}", message);
                writer.send(message).await?;
            }
            _ => {
                return Err("No websocket writer".into());
            }
        }
        Ok(())
    }
}

/// Configuration for a [`Streamer`].
pub struct StreamerConfig {
    pub id: String,
    pub name: String,
    pub address: String,
    pub port: u16,
    pub tun_ip_network: String,
    pub password: String,
    pub destination_address: String,
    pub destination_port: u16,
}

struct StreamerInner {
    me: Weak<Mutex<Self>>,
    id: String,
    name: String,
    address: String,
    port: u16,
    password: String,
    relays: Vec<Arc<Mutex<Relay>>>,
    unique_indexes: Vec<u32>,
    tun_ip_network: Ipv4Network,
    service_daemon: ServiceDaemon,
    destination_address: String,
    destination_port: u16,
}

impl StreamerInner {
    pub fn new(
        config: StreamerConfig,
    ) -> Result<Arc<Mutex<Self>>, Box<dyn std::error::Error + Send + Sync>> {
        let tun_ip_network = parse_tun_ip_network(&config.tun_ip_network)?;
        Ok(Arc::new_cyclic(|me| {
            Mutex::new(Self {
                me: me.clone(),
                id: config.id,
                name: config.name,
                address: config.address,
                port: config.port,
                password: config.password,
                relays: Vec::new(),
                unique_indexes: (1..tun_ip_network.size() - 1).rev().collect(),
                tun_ip_network,
                service_daemon: Self::create_service_daemon(),
                destination_address: config.destination_address,
                destination_port: config.destination_port,
            })
        }))
    }

    pub async fn start(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.start_relay_listener().await?;
        self.start_mdns_daemon();
        Ok(())
    }

    fn create_service_daemon() -> ServiceDaemon {
        let service_daemon = ServiceDaemon::new().unwrap();
        service_daemon
            .disable_interface(Vec::from([IfKind::IPv6]))
            .ok();
        service_daemon
    }

    async fn start_relay_listener(
        &mut self,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let listener_address = format!("{}:{}", self.address, self.port);
        let listener = TcpListener::bind(&listener_address).await?;
        info!("WebSocket server listening on '{}'", listener_address);
        info!("TUN devices will be created when relays connect");
        let streamer = self.me.clone();

        tokio::spawn(async move {
            while let Ok((tcp_stream, relay_address)) = listener.accept().await {
                info!("New relay connection from: {}", relay_address);
                match streamer.upgrade() {
                    Some(streamer) => {
                        streamer
                            .lock()
                            .await
                            .handle_relay_connection(tcp_stream, relay_address)
                            .await;
                    }
                    _ => {
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    fn start_mdns_daemon(&mut self) {
        match self.create_mdns_service_info() {
            Ok(service_info) => {
                if let Err(error) = self.service_daemon.register(service_info) {
                    error!("Failed to register mDNS service with error: {}", error);
                }
            }
            Err(error) => {
                error!("Failed to create mDNS service info with error: {}", error);
            }
        }
    }

    fn create_mdns_service_info(&self) -> Result<ServiceInfo, AnyError> {
        let properties = HashMap::from([("name".to_string(), self.name.clone())]);
        let service_info = ServiceInfo::new(
            MDNS_SERVICE_TYPE,
            &self.id,
            &format!("{}.local.", self.id),
            "",
            self.port,
            properties,
        )?
        .enable_addr_auto();
        Ok(service_info)
    }

    async fn handle_relay_connection(&mut self, tcp_stream: TcpStream, relay_address: SocketAddr) {
        use http::header::{ORIGIN, SEC_WEBSOCKET_PROTOCOL, USER_AGENT};
        use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

        let callback = |req: &Request, response: Response| {
            let mut response = response;
            let headers = response.headers_mut();
            headers.insert(
                SEC_WEBSOCKET_PROTOCOL,
                http::HeaderValue::from_static("moblink"),
            );
            headers.insert(
                ORIGIN,
                http::HeaderValue::from_static("moblin://streamer"),
            );
            headers.insert(
                USER_AGENT,
                http::HeaderValue::from_static("Moblin/1.0"),
            );
            Ok(response)
        };

        match tokio_tungstenite::accept_hdr_async(tcp_stream, callback).await {
            Ok(websocket_stream) => {
                info!("Relay connected: {}", relay_address);
                let (writer, reader) = websocket_stream.split();
                let Some(unique_index) = self.unique_indexes.pop() else {
                    warn!("No unique index available for relay");
                    return;
                };
                let Some(tun_ip_address) = self.tun_ip_network.nth(unique_index) else {
                    warn!("No TUN IP available for index: {}", unique_index);
                    self.unique_indexes.insert(0, unique_index);
                    return;
                };
                info!("Assigning TUN IP {} to relay", tun_ip_address);
                let relay = Relay::new(
                    self.me.clone(),
                    relay_address,
                    writer,
                    tun_ip_address.to_string(),
                    unique_index,
                    self.destination_address.clone(),
                    self.destination_port,
                );
                relay.lock().await.start(reader);
                self.add_relay(relay);
            }
            Err(error) => {
                error!("Relay websocket handshake failed with: {}", error);
            }
        }
    }

    fn add_relay(&mut self, relay: Arc<Mutex<Relay>>) {
        self.relays.push(relay);
        self.log_number_of_relays();
    }

    async fn remove_relay(&mut self, relay: &Arc<Mutex<Relay>>) {
        let unique_index = relay.lock().await.unique_index;
        self.unique_indexes.insert(0, unique_index);
        self.relays.retain(|r| !Arc::ptr_eq(r, relay));
        self.log_number_of_relays();
    }

    fn log_number_of_relays(&self) {
        info!("Number of relays: {}", self.relays.len())
    }
}

pub struct Streamer {
    inner: Arc<Mutex<StreamerInner>>,
}

impl Streamer {
    pub fn new(config: StreamerConfig) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self {
            inner: StreamerInner::new(config)?,
        })
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.inner.lock().await.start().await
    }
}

fn parse_tun_ip_network(network: &str) -> Result<Ipv4Network, AnyError> {
    let network: Ipv4Network = network.parse()?;
    if network.size() > 256 {
        return Err(format!("TUN IP network too big ({} > 256)", network.size()).into());
    }
    Ok(network)
}
