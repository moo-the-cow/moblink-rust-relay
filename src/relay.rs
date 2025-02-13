use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;

use base64::engine::general_purpose;
use base64::Engine as _;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use log::{debug, error, info};
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{self};
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use crate::protocol::*;

pub struct Relay {
    /// Store a local IP address  for binding UDP sockets
    bind_address: String,
    relay_id: String,
    streamer_url: String,
    password: String,
    name: String,
    on_status_updated: Option<Box<dyn Fn(String) + Send + Sync>>,
    #[allow(clippy::type_complexity)]
    get_battery_percentage: Option<Arc<dyn Fn(Box<dyn FnOnce(Option<i32>)>) + Send + Sync>>,
    ws_in: Option<Arc<Mutex<SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>>>>,
    started: bool,
    connected: bool,
    wrong_password: bool,
}

impl Relay {
    pub fn new() -> Self {
        Self {
            bind_address: Self::get_default_bind_address(),
            relay_id: "".to_string(),
            streamer_url: "".to_string(),
            password: "".to_string(),
            name: "".to_string(),
            on_status_updated: None,
            get_battery_percentage: None,
            ws_in: None,
            started: false,
            connected: false,
            wrong_password: false,
        }
    }

    fn get_default_bind_address() -> String {
        // Get main network interface
        let interfaces = pnet::datalink::interfaces();
        let interface = interfaces.iter().find(|interface| {
            interface.is_up() && !interface.is_loopback() && !interface.ips.is_empty()
        });

        let interface = match interface {
            Some(interface) => interface,
            None => {
                panic!("No available network interfaces found");
            }
        };

        // Only ipv4 addresses are supported
        let ipv4_addresses: Vec<String> = interface
            .ips
            .iter()
            .filter_map(|ip| {
                let ip = ip.ip();
                ip.is_ipv4().then(|| ip.to_string())
            })
            .collect();

        // Return the first address
        ipv4_addresses
            .get(0)
            .cloned()
            .unwrap_or_else(|| "0.0.0.0:0".to_string())
    }

    #[allow(dead_code)]
    pub fn set_bind_address(&mut self, address: String) {
        self.bind_address = address;
    }

    pub async fn setup<F, G>(
        &mut self,
        streamer_url: String,
        password: String,
        relay_id: String,
        name: String,
        on_status_updated: F,
        get_battery_percentage: G,
    ) where
        F: Fn(String) + Send + Sync + 'static,
        G: Fn(Box<dyn FnOnce(Option<i32>)>) + Send + Sync + 'static,
    {
        self.on_status_updated = Some(Box::new(on_status_updated));
        self.get_battery_percentage = Some(Arc::new(get_battery_percentage));
        self.relay_id = relay_id;
        self.streamer_url = streamer_url;
        self.password = password;
        self.name = name;
        info!("Binding to address: {:?}", self.bind_address);
    }

    pub fn is_started(&self) -> bool {
        self.started
    }

    pub async fn start(relay_arc: Arc<Mutex<Self>>) {
        let mut relay = relay_arc.lock().await;
        if !relay.started {
            relay.started = true;
            drop(relay);
            Self::start_internal(relay_arc.clone()).await;
        }
    }

    #[allow(dead_code)]
    pub async fn stop(relay_arc: Arc<Mutex<Self>>) {
        let mut relay = relay_arc.lock().await;
        if relay.started {
            relay.started = false;
            relay.stop_internal().await;
        }
    }

    #[allow(dead_code)]
    pub async fn update_settings(
        &mut self,
        relay_id: String,
        streamer_url: String,
        password: String,
        name: String,
    ) {
        self.relay_id = relay_id;
        self.streamer_url = streamer_url;
        self.password = password;
        self.name = name;
    }

    async fn start_internal(relay_arc: Arc<Mutex<Self>>) {
        let relay_id: String;
        let streamer_url: String;
        let password: String;
        let name: String;
        let get_battery_percentage: Option<
            Arc<dyn Fn(Box<dyn FnOnce(Option<i32>)>) + Send + Sync>,
        >;
        {
            let mut relay = relay_arc.lock().await;
            if !relay.started {
                relay.stop_internal().await;
                return;
            }

            relay_id = relay.relay_id.clone();
            streamer_url = relay.streamer_url.clone();
            password = relay.password.clone();
            name = relay.name.clone();
            get_battery_percentage = relay.get_battery_percentage.clone();

            if !relay.started {
                return;
            }
        }

        let request = match url::Url::parse(&streamer_url) {
            Ok(url) => url,
            Err(e) => {
                error!("Failed to parse URL: {}", e);
                return;
            }
        };

        let (tx, mut rx) =
            mpsc::channel::<Result<Message, tokio_tungstenite::tungstenite::Error>>(32);
        match tokio::time::timeout(Duration::from_secs(10), connect_async(request.to_string()))
            .await
        {
            Ok(Ok((ws_stream, _))) => {
                info!("WebSocket connected");
                let (write, mut read) = ws_stream.split();
                {
                    let mut relay = relay_arc.lock().await;
                    relay.ws_in = Some(Arc::new(Mutex::new(write)));
                }
                // Task to handle incoming messages
                tokio::spawn(async move {
                    while let Some(message) = read.next().await {
                        if let Err(e) = tx.send(message).await {
                            error!("Failed to send message over channel: {}", e);
                            break;
                        }
                    }
                });
            }

            Ok(Err(e)) => {
                // This means the future completed but the connection failed
                error!("WebSocket connection failed immediately: {}", e);
                let relay_arc_clone = relay_arc.clone();
                Self::reconnect_soon(relay_arc_clone).await;
                return;
            }
            Err(_elapsed) => {
                // This means the future did NOT complete within 10 seconds
                error!("WebSocket connection attempt timed out after 10 seconds");
                let relay_arc_clone = relay_arc.clone();
                Self::reconnect_soon(relay_arc_clone).await;
                return;
            }
        };

        let ws_in_clone = relay_arc.lock().await.ws_in.clone();

        if ws_in_clone.is_none() {
            error!("Failed to acquire lock to clone WebSocket");
            return;
        }

        if let Some(ws_in_clone) = ws_in_clone {
            // Task to process messages received from the channel.
            tokio::spawn(async move {
                while let Some(result) = rx.recv().await {
                    match result {
                        Ok(msg) => match msg {
                            Message::Text(text) => {
                                info!("Received text message: {}", text);
                                if let Ok(deserialized) =
                                    serde_json::from_str::<MessageToRelay>(&text)
                                {
                                    match Self::handle_message(
                                        relay_arc.clone(),
                                        ws_in_clone.clone(),
                                        deserialized,
                                        password.clone(),
                                        name.clone(),
                                        relay_id.clone(),
                                        get_battery_percentage.as_deref(),
                                    )
                                    .await
                                    {
                                        Ok(_) => info!("Message handled successfully"),
                                        Err(e) => error!("Error handling message: {}", e),
                                    };
                                } else {
                                    error!("Failed to deserialize message: {}", text);
                                }
                            }
                            Message::Binary(data) => {
                                debug!("Received binary message of length: {}", data.len());
                            }
                            Message::Ping(_) => {
                                debug!("Received ping message");
                            }
                            Message::Pong(_) => {
                                debug!("Received pong message");
                            }
                            Message::Close(frame) => {
                                info!("Received close message: {:?}", frame);
                                break;
                            }
                            Message::Frame(_) => {
                                unreachable!("This is never used")
                            }
                        },
                        Err(e) => {
                            error!("Error processing message: {}", e);
                            // TODO: There has to be a better way to handle this
                            if e.to_string()
                                .contains("Connection reset without closing handshake")
                            {
                                let relay_arc_clone = relay_arc.clone();
                                Self::reconnect_soon(relay_arc_clone).await;
                            }
                            break;
                        }
                    }
                }
            });
        }
    }

    async fn stop_internal(&mut self) {
        info!("Stopping internal processes");
        if let Some(ws_in) = &self.ws_in {
            if let Ok(mut locked_ws) = ws_in.try_lock() {
                if let Err(e) = locked_ws.close().await {
                    error!("Error closing WebSocket: {}", e);
                } else {
                    info!("WebSocket closed successfully");
                }
            } else {
                error!("Failed to acquire lock to close WebSocket");
            }
            self.ws_in = None;
        }
        self.connected = false;
        self.wrong_password = false;
        self.update_status_internal().await;
    }

    async fn update_status_internal(&self) {
        let status = if self.connected {
            "Connected to streamer".to_string()
        } else if self.wrong_password {
            "Wrong password".to_string()
        } else if self.started {
            "Connecting to streamer".to_string()
        } else {
            "Disconnected from streamer".to_string()
        };
        if let Some(on_status_updated) = &self.on_status_updated {
            on_status_updated(status);
        }
    }

    fn reconnect_soon(relay_arc: Arc<Mutex<Self>>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            {
                let mut relay = relay_arc.lock().await;
                relay.stop_internal().await;
            }

            info!("Reconnecting in 5 seconds...");
            sleep(Duration::from_secs(5)).await;

            Self::start_internal(relay_arc.clone()).await;
        })
    }

    async fn handle_message(
        relay_arc: Arc<Mutex<Self>>,
        ws_in: Arc<
            Mutex<
                futures_util::stream::SplitSink<
                    WebSocketStream<MaybeTlsStream<TcpStream>>,
                    Message,
                >,
            >,
        >,
        message: MessageToRelay,
        password: String,
        name: String,
        relay_id: String,
        get_battery_percentage: Option<&(dyn Fn(Box<dyn FnOnce(Option<i32>)>) + Send + Sync)>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        match message {
            MessageToRelay::Hello(hello) => {
                let authentication = calculate_authentication(
                    &password,
                    &hello.authentication.salt,
                    &hello.authentication.challenge,
                );
                let identify = Identify {
                    id: relay_id,
                    name,
                    authentication,
                };
                let message = MessageToStreamer::Identify(identify);
                let text = serde_json::to_string(&message)?;
                let mut locked_ws_in = ws_in.lock().await;
                info!("Sending identify message: {}", text);
                locked_ws_in.send(Message::Text(text.into())).await?;
                Ok(())
            }
            MessageToRelay::Identified(identified) => {
                info!("Received identified message: {:?}", identified);
                let mut relay = relay_arc.lock().await;
                match identified.result {
                    MoblinkResult::Ok(_) => {
                        relay.connected = true;
                    }
                    MoblinkResult::WrongPassword(_) => {
                        relay.wrong_password = true;
                    }
                }
                relay.update_status_internal().await;
                Ok(())
            }
            MessageToRelay::Request(request) => match request.data {
                MessageRequestData::StartTunnel(start_tunnel) => {
                    info!("Received start tunnel request: {:?}", start_tunnel);
                    let request_id = request.id;
                    let cloned_ws_in = ws_in.clone();
                    tokio::spawn(async move {
                        match handle_start_tunnel_request(
                            relay_arc,
                            cloned_ws_in,
                            request_id,
                            start_tunnel.address,
                            start_tunnel.port,
                        )
                        .await
                        {
                            Ok(_) => info!("Start tunnel request handled successfully."),
                            Err(e) => error!("Error handling start tunnel request: {}", e),
                        };
                    });
                    Ok(())
                }
                MessageRequestData::Status(_) => {
                    if let Some(get_battery_percentage) = get_battery_percentage {
                        info!("Handling status request");
                        get_battery_percentage(Box::new(move |battery_percentage| {
                            let data =
                                ResponseData::Status(StatusResponseData { battery_percentage });
                            let response = MessageResponse {
                                id: request.id,
                                result: MoblinkResult::Ok(Present {}),
                                data,
                            };
                            let message = MessageToStreamer::Response(response);
                            let text = serde_json::to_string(&message);

                            if let Err(e) = text {
                                error!("Failed to serialize status response: {}", e);
                                return;
                            }

                            if let Ok(text) = text {
                                log::info!("Sending status response: {}", text);
                                let cloned_ws_in = ws_in.clone();
                                tokio::spawn(async move {
                                    let mut locked_ws_in = cloned_ws_in.lock().await;
                                    match locked_ws_in.send(Message::Text(text.into())).await {
                                        Ok(_) => info!("Status response sent successfully."),
                                        Err(e) => error!("Failed to send status response: {}", e),
                                    }
                                });
                            }
                        }));
                        Ok(())
                    } else {
                        error!("get_battery_percentage is not set");
                        Err("get_battery_percentage function not set".into())
                    }
                }
            },
        }
    }
}

fn calculate_authentication(password: &str, salt: &str, challenge: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{}{}", password, salt).as_bytes());
    let hash1 = hasher.finalize_reset();
    hasher.update(format!("{}{}", general_purpose::STANDARD.encode(hash1), challenge).as_bytes());
    let hash2 = hasher.finalize();
    general_purpose::STANDARD.encode(hash2)
}

async fn create_dual_stack_udp_socket(
    addr: SocketAddr,
) -> Result<tokio::net::UdpSocket, std::io::Error> {
    let socket = match addr.is_ipv4() {
        true => {
            // Create an IPv4 socket
            tokio::net::UdpSocket::bind(addr).await?
        }
        false => {
            // Create a dual-stack socket (supporting both IPv4 and IPv6)
            let socket = socket2::Socket::new(
                socket2::Domain::IPV6,
                socket2::Type::DGRAM,
                Some(socket2::Protocol::UDP),
            )?;

            // Set IPV6_V6ONLY to false to enable dual-stack support
            socket.set_only_v6(false)?;

            // Bind the socket
            socket.bind(&socket2::SockAddr::from(addr))?;

            // Convert to a tokio UdpSocket
            tokio::net::UdpSocket::from_std(socket.into())?
        }
    };

    Ok(socket)
}

// Helper function to parse a string into a SocketAddr, handling IP addresses
// without ports.
fn parse_socket_addr(addr_str: &str) -> Result<SocketAddr, std::io::Error> {
    // Attempt to parse the string as a full SocketAddr (IP:port)
    if let Ok(socket_addr) = SocketAddr::from_str(addr_str) {
        return Ok(socket_addr);
    }

    // If parsing as SocketAddr fails, try parsing as IP address and append default
    // port
    if let Ok(ip_addr) = IpAddr::from_str(addr_str) {
        // Use 0 as the default port, allowing the OS to assign an available port
        return Ok(SocketAddr::new(ip_addr, 0));
    }

    // Return an error if both attempts fail
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "Invalid socket address syntax. Expected 'IP:port' or 'IP'.",
    ))
}

async fn handle_start_tunnel_request(
    relay_arc: Arc<Mutex<Relay>>,
    ws_in: Arc<Mutex<SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>>>,
    request_id: u32,
    destination_ip: String,
    destination_port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Pick bind addresses from the relay
    let (local_bind_addr_for_streamer, local_bind_addr_for_destination) = {
        let relay = relay_arc.lock().await;
        let addr = relay.bind_address.clone();
        (parse_socket_addr("0.0.0.0")?, parse_socket_addr(&addr)?)
    };

    info!(
        "Binding streamer socket on: {}, destination socket on: {}",
        local_bind_addr_for_streamer, local_bind_addr_for_destination
    );
    // Create a UDP socket bound for receiving packets from the server.
    // Use dual-stack socket creation.
    let streamer_socket = create_dual_stack_udp_socket(local_bind_addr_for_streamer).await?;
    let streamer_port = streamer_socket.local_addr()?.port();
    info!("Listening on UDP port: {}", streamer_port);
    let streamer_socket = Arc::new(streamer_socket);

    // Inform the server about the chosen port.
    let data = ResponseData::StartTunnel(StartTunnelResponseData {
        port: streamer_port,
    });
    let response = MessageResponse {
        id: request_id,
        result: MoblinkResult::Ok(Present {}),
        data,
    };
    let message = MessageToStreamer::Response(response);
    let text = serde_json::to_string(&message)?;
    {
        let mut locked_ws_in = ws_in.lock().await;
        info!("Sending start tunnel response: {}", text);
        locked_ws_in.send(Message::Text(text.into())).await?;
    }

    // Create a new UDP socket for communication with the destination.
    // Use dual-stack socket creation.
    let destination_socket = create_dual_stack_udp_socket(local_bind_addr_for_destination).await?;

    info!(
        "Bound destination socket to: {:?}",
        destination_socket.local_addr()?
    );
    let destination_socket = Arc::new(destination_socket);

    let destination_socket_clone = destination_socket.clone();
    let streamer_socket_clone = streamer_socket.clone();

    let parsed_ip = IpAddr::from_str(&destination_ip)?;
    let normalized_ip = match parsed_ip {
        IpAddr::V4(v4) => IpAddr::V4(v4),
        IpAddr::V6(v6) => {
            // If it’s an IPv4-mapped IPv6 like ::ffff:x.x.x.x, convert to real IPv4
            if let Some(mapped_v4) = v6.to_ipv4() {
                IpAddr::V4(mapped_v4)
            } else {
                // Otherwise, keep it as IPv6
                IpAddr::V6(v6)
            }
        }
    };
    let destination_addr = SocketAddr::new(normalized_ip, destination_port);
    info!("Destination address resolved: {}", destination_addr);

    // Use an Arc<Mutex> to share the server_remote_addr between tasks.
    let server_remote_addr: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));

    // Relay packets from streamer to destination.
    let relay_to_destination: tokio::task::JoinHandle<()> = {
        let server_remote_addr_clone = server_remote_addr.clone();
        tokio::spawn(async move {
            debug!("(relay_to_destination) Task started");
            loop {
                let mut buf = [0; 2048];
                let (size, remote_addr) = match tokio::time::timeout(
                    Duration::from_secs(30),
                    streamer_socket_clone.recv_from(&mut buf),
                )
                .await
                {
                    Ok(result) => match result {
                        Ok((size, addr)) => (size, addr),
                        Err(e) => {
                            error!("(relay_to_destination) Error receiving from server: {}", e);
                            continue;
                        }
                    },
                    Err(e) => {
                        error!(
                            "(relay_to_destination) Timeout receiving from server: {}",
                            e
                        );
                        continue;
                    }
                };

                debug!(
                    "(relay_to_destination) Received {} bytes from server: {}",
                    size, remote_addr
                );
                // Forward to destination.
                match destination_socket_clone
                    .send_to(&buf[..size], &destination_addr)
                    .await
                {
                    Ok(bytes_sent) => {
                        debug!(
                            "(relay_to_destination) Sent {} bytes to destination",
                            bytes_sent
                        )
                    }
                    Err(e) => {
                        error!(
                            "(relay_to_destination) Failed to send to destination: {}",
                            e
                        );
                        break;
                    }
                }

                // Set the remote address if it hasn't been set yet.
                let mut server_remote_addr_lock = server_remote_addr_clone.lock().await;
                if server_remote_addr_lock.is_none() {
                    *server_remote_addr_lock = Some(remote_addr);
                    debug!(
                        "(relay_to_destination) Server remote address set to: {}",
                        remote_addr
                    );
                }
            }
            info!("(relay_to_destination) Task exiting");
        })
    };
    // Relay packets from destination to streamer.
    let relay_to_streamer = {
        let server_remote_addr_clone = server_remote_addr.clone();
        tokio::spawn(async move {
            debug!("(relay_to_streamer) Task started");
            loop {
                let mut buf = [0; 2048];
                let (size, remote_addr) = match tokio::time::timeout(
                    Duration::from_secs(30),
                    destination_socket.recv_from(&mut buf),
                )
                .await
                {
                    Ok(result) => match result {
                        Ok((size, addr)) => (size, addr),
                        Err(e) => {
                            error!(
                                "(relay_to_streamer) Error receiving from destination: {}",
                                e
                            );
                            continue;
                        }
                    },
                    Err(e) => {
                        error!(
                            "(relay_to_streamer) Timeout receiving from destination: {}",
                            e
                        );
                        continue;
                    }
                };

                debug!(
                    "(relay_to_streamer) Received {} bytes from destination: {}",
                    size, remote_addr
                );
                // Forward to server.
                let server_remote_addr_lock = server_remote_addr_clone.lock().await;
                match *server_remote_addr_lock {
                    Some(server_addr) => {
                        match streamer_socket.send_to(&buf[..size], &server_addr).await {
                            Ok(bytes_sent) => {
                                debug!("(relay_to_streamer) Sent {} bytes to server", bytes_sent)
                            }
                            Err(e) => {
                                error!("(relay_to_streamer) Failed to send to server: {}", e);
                                break;
                            }
                        }
                    }
                    None => {
                        error!("(relay_to_streamer) Server address not set, cannot forward packet");
                    }
                }
            }
            info!("(relay_to_streamer) Task exiting");
        })
    };

    // Wait for relay tasks to complete (they won't unless an error occurs or the
    // socket is closed).
    tokio::select! {
        res = relay_to_destination => {
            if let Err(e) = res {
                error!("relay_to_destination task failed: {}", e);
            }
        }
        res = relay_to_streamer => {
            if let Err(e) = res {
                error!("relay_to_streamer task failed: {}", e);
            }
        }
    }

    Ok(())
}
