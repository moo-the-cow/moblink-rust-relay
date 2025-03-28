use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug)]
pub struct Present {}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub enum MoblinkResult {
    Ok(Present),
    WrongPassword(Present),
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
#[serde(rename_all = "camelCase")]
pub enum MessageRequestData {
    StartTunnel(StartTunnelRequest),
    Status(Present),
}

#[derive(Deserialize, Debug)]
pub struct MessageRequest {
    pub id: u32,
    pub data: MessageRequestData,
}

impl MessageRequest {
    pub fn to_ok_response(&self, data: ResponseData) -> MessageResponse {
        MessageResponse {
            id: self.id,
            result: MoblinkResult::Ok(Present {}),
            data,
        }
    }
}

#[derive(Serialize, Debug)]
pub struct StartTunnelResponseData {
    pub port: u16,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StatusResponseData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battery_percentage: Option<i32>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub enum ResponseData {
    StartTunnel(StartTunnelResponseData),
    Status(StatusResponseData),
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
#[serde(rename_all = "camelCase")]
pub enum MessageToRelay {
    Hello(Hello),
    Identified(Identified),
    Request(MessageRequest),
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub enum MessageToStreamer {
    Identify(Identify),
    Response(MessageResponse),
}
