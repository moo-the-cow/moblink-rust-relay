use crate::protocol::*;
use base64::{engine::general_purpose, Engine as _};
use futures_util::{stream::SplitSink, SinkExt, StreamExt};
use log::{debug, error, info};
use sha2::{Digest, Sha256};
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::{
    mpsc::{self},
    Mutex,
};
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{
    connect_async, tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream,
};
use uuid;

pub struct Relay {
    relay_id: String,
    streamer_url: String,
    password: String,
    name: String,
    on_status_updated: Option<Box<dyn Fn(String) + Send + Sync>>,
    #[allow(clippy::type_complexity)]
    get_battery_percentage: Option<Arc<dyn Fn(Box<dyn FnOnce(i32)>) + Send + Sync>>,
    ws_in: Option<Arc<Mutex<SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>>>>,
    started: bool,
    connected: bool,
    wrong_password: bool,
}

impl Relay {
    pub fn new() -> Self {
        Self {
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

    pub fn generate_relay_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }

    pub async fn setup<F, G>(
        &mut self,
        streamer_url: String,
        password: String,
        name: String,
        on_status_updated: F,
        get_battery_percentage: G,
    ) where
        F: Fn(String) + Send + Sync + 'static,
        G: Fn(Box<dyn FnOnce(i32)>) + Send + Sync + 'static,
    {
        self.on_status_updated = Some(Box::new(on_status_updated));
        self.get_battery_percentage = Some(Arc::new(get_battery_percentage));
        self.relay_id = self.generate_relay_id();
        self.streamer_url = streamer_url;
        self.password = password;
        self.name = name;
        self.update_status_internal().await;
    }

    pub async fn start(relay_arc: Arc<Mutex<Self>>) {
        let mut relay = relay_arc.lock().await;
        if !relay.started {
            relay.started = true;
            drop(relay); // Explicitly drop the lock before calling `start_internal`
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
        let get_battery_percentage: Option<Arc<dyn Fn(Box<dyn FnOnce(i32)>) + Send + Sync>>;
        // Clone the `Arc` for use in the task
        {
            let mut relay = relay_arc.lock().await;
            relay.stop_internal().await;

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
        match connect_async(request).await {
            Ok((ws_stream, _)) => {
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
            Err(e) => {
                error!("WebSocket connection failed: {}", e);

                // TODO: Implement proper reconnection logic here
                /*let relay_arc_clone = relay_arc.clone();
                tokio::spawn(async move {
                    Self::reconnect_soon(relay_arc_clone).await;
                });*/
            }
        };

        let ws_in_clone = relay_arc.lock().await.ws_in.clone().unwrap();

        // Task to process messages received from the channel.
        tokio::spawn(async move {
            while let Some(result) = rx.recv().await {
                match result {
                    Ok(msg) => match msg {
                        Message::Text(text) => {
                            info!("Received text message: {}", text);
                            if let Ok(deserialized) = serde_json::from_str::<MessageToClient>(&text)
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
                        // Implement proper reconnection logic here
                    }
                }
            }
        });
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

    #[allow(dead_code)]
    async fn reconnect_soon(relay_arc: Arc<Mutex<Self>>) {
        {
            let mut relay = relay_arc.lock().await;
            relay.stop_internal().await;
        }
        info!("Reconnecting in 5 seconds...");
        sleep(Duration::from_secs(5)).await;
        Self::start_internal(relay_arc.clone()).await;
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
        message: MessageToClient,
        password: String,
        name: String,
        relay_id: String,
        get_battery_percentage: Option<&(dyn Fn(Box<dyn FnOnce(i32)>) + Send + Sync)>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(hello) = message.hello {
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
            let message = MessageToServer {
                identify: Some(identify),
                response: None,
            };
            let text = serde_json::to_string(&message)?;
            let mut locked_ws_in = ws_in.lock().await;
            info!("Sending identify message: {}", text);
            locked_ws_in.send(Message::Text(text)).await?;
            Ok(())
        } else if let Some(identified) = message.identified {
            info!("Received identified message: {:?}", identified);
            let mut relay = relay_arc.lock().await;
            if identified.result.ok.is_some() {
                relay.connected = true;
            } else if identified.result.wrong_password.is_some() {
                relay.wrong_password = true;
            }
            relay.update_status_internal().await;
            Ok(())
        } else if let Some(request) = message.request {
            if let Some(start_tunnel) = request.data.start_tunnel {
                info!("Received start tunnel request: {:?}", start_tunnel);
                let request_id = request.id;
                let cloned_ws_in = ws_in.clone();
                tokio::spawn(async move {
                    match handle_start_tunnel_request(
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
            } else if request.data.status.is_some() {
                if let Some(get_battery_percentage) = get_battery_percentage {
                    info!("Handling status request");
                    get_battery_percentage(Box::new(move |battery_percentage| {
                        let data = ResponseData {
                            start_tunnel: None,
                            status: Some(StatusResponseData {
                                battery_percentage: Some(battery_percentage),
                            }),
                        };
                        let response = MessageResponse {
                            id: request.id,
                            result: MoblinkResult {
                                ok: Some(Present {}),
                                wrong_password: None,
                            },
                            data,
                        };
                        let message = MessageToServer {
                            identify: None,
                            response: Some(response),
                        };
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
                                match locked_ws_in.send(Message::Text(text)).await {
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
            } else {
                error!("Received unknown request: {:?}", request);
                Ok(())
            }
        } else {
            error!("Received unknown message: {:?}", message);
            Ok(())
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

async fn handle_start_tunnel_request(
    ws_in: Arc<Mutex<SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>>>,
    request_id: u32,
    destination_ip: String,
    destination_port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Create a UDP socket bound to a random port for receiving packets from the server.
    let streamer_socket = UdpSocket::bind("0.0.0.0:0")?;
    let streamer_port = streamer_socket.local_addr()?.port();
    info!("Listening on UDP port: {}", streamer_port);
    let streamer_socket = Arc::new(tokio::net::UdpSocket::from_std(streamer_socket)?);

    // Inform the server about the chosen port.
    let data = ResponseData {
        start_tunnel: Some(StartTunnelResponseData {
            port: streamer_port,
        }),
        status: None,
    };
    let response = MessageResponse {
        id: request_id,
        result: MoblinkResult {
            ok: Some(Present {}),
            wrong_password: None,
        },
        data,
    };
    let message = MessageToServer {
        identify: None,
        response: Some(response),
    };
    let text = serde_json::to_string(&message)?;
    {
        let mut locked_ws_in = ws_in.lock().await;
        info!("Sending start tunnel response: {}", text);
        locked_ws_in.send(Message::Text(text)).await?;
    } // Mutex lock is released here

    // Create a new UDP socket for communication with the destination.
    let destination_socket = UdpSocket::bind("0.0.0.0:0")?;
    info!(
        "Bound destination socket to: {:?}",
        destination_socket.local_addr()?
    ); // Added logging
    let destination_socket = Arc::new(tokio::net::UdpSocket::from_std(destination_socket)?);

    let destination_socket_clone = destination_socket.clone();
    let streamer_socket_clone = streamer_socket.clone();
    let destination_addr = format!("{}:{}", destination_ip, destination_port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| {
            error!(
                "Failed to resolve destination address: {}:{}",
                destination_ip, destination_port
            );
            "Failed to resolve destination address"
        })?;

    info!("Destination address resolved: {}", destination_addr); // Added logging

    let mut server_remote_addr: Option<SocketAddr> = None;

    // Relay packets from streamer to destination.
    let relay_to_destination = tokio::spawn(async move {
        let mut buf = [0; 2048];
        debug!("(relay_to_destination) Task started");
        loop {
            let (size, remote_addr) = match tokio::time::timeout(
                Duration::from_secs(5),
                streamer_socket_clone.recv_from(&mut buf),
            )
            .await
            {
                Ok(result) => {
                    match result {
                        Ok((size, addr)) => (size, addr),
                        Err(e) => {
                            error!("(relay_to_destination) Error receiving from server: {}", e);
                            continue; // Continue to the next iteration after error
                        }
                    }
                }
                Err(e) => {
                    error!(
                        "(relay_to_destination) Timeout receiving from server: {}",
                        e
                    );
                    continue; // Continue to the next iteration after timeout
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
            // Check if this is the first packet and set the remote address
            if server_remote_addr.is_none() {
                server_remote_addr = Some(remote_addr);
                debug!(
                    "(relay_to_destination) Server remote address set to: {}",
                    remote_addr
                );
            }
        }
        info!("(relay_to_destination) Task exiting");
    });

    // Relay packets from destination to streamer.
    let relay_to_streamer = tokio::spawn(async move {
        let mut buf = [0; 2048];
        debug!("(relay_to_streamer) Task started");
        loop {
            let (size, remote_addr) = match tokio::time::timeout(
                Duration::from_secs(5),
                destination_socket.recv_from(&mut buf),
            )
            .await
            {
                Ok(result) => {
                    match result {
                        Ok((size, addr)) => (size, addr),
                        Err(e) => {
                            error!(
                                "(relay_to_streamer) Error receiving from destination: {}",
                                e
                            );
                            continue; // Continue to the next iteration after error
                        }
                    }
                }
                Err(e) => {
                    error!(
                        "(relay_to_streamer) Timeout receiving from destination: {}",
                        e
                    );
                    continue; // Continue to the next iteration after timeout
                }
            };

            debug!(
                "(relay_to_streamer) Received {} bytes from destination: {}",
                size, remote_addr
            );
            // Forward to server.
            match server_remote_addr {
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
    });

    // Wait for relay tasks to complete (they won't unless an error occurs or the socket is closed).
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
