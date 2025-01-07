use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug)]
pub struct Present {
    pub(crate) dummy: Option<bool>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct MoblinkResult {
    pub ok: Option<Present>,
    #[serde(rename = "wrongPassword")]
    pub wrong_password: Option<Present>,
}

#[derive(Deserialize, Debug)]
pub struct Authentication {
    pub challenge: String,
    pub salt: String,
}

#[derive(Deserialize, Debug)]
pub struct Hello {
    #[serde(rename = "apiVersion")]
    #[allow(dead_code)]
    pub api_version: String,
    pub authentication: Authentication,
}

#[derive(Deserialize, Debug)]
pub struct Identified {
    pub result: MoblinkResult,
}

#[derive(Deserialize, Debug)]
pub struct StartTunnelRequest {
    pub address: String,
    pub port: u16,
}

#[derive(Deserialize, Debug)]
pub struct MessageRequestData {
    #[serde(rename = "startTunnel")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_tunnel: Option<StartTunnelRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Present>,
}

#[derive(Deserialize, Debug)]
pub struct MessageRequest {
    pub id: u32,
    pub data: MessageRequestData,
}

#[derive(Serialize, Debug)]
pub struct StartTunnelResponseData {
    pub port: u16,
}

#[derive(Serialize, Debug)]
pub struct StatusResponseData {
    #[serde(rename = "batteryPercentage")]
    pub battery_percentage: Option<i32>,
}

#[derive(Serialize, Debug)]
pub struct ResponseData {
    #[serde(rename = "startTunnel")]
    pub start_tunnel: Option<StartTunnelResponseData>,
    pub status: Option<StatusResponseData>,
}

#[derive(Serialize, Debug)]
pub struct MessageResponse {
    pub id: u32,
    pub result: MoblinkResult,
    pub data: ResponseData,
}

#[derive(Serialize, Debug)]
pub struct Identify {
    pub id: String,
    pub name: String,
    pub authentication: String,
}

#[derive(Deserialize, Debug)]
pub struct MessageToClient {
    pub hello: Option<Hello>,
    pub identified: Option<Identified>,
    pub request: Option<MessageRequest>,
}

#[derive(Serialize, Debug)]
pub struct MessageToServer {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identify: Option<Identify>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<MessageResponse>,
}
