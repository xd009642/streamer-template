use crate::model;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StartMessage {
    trace_id: Option<String>,
    channels: usize,
    sample_rate: usize,
}

// TODO we might want to do a base64 message here, I dislike the APIs but some people don't realise
// you can send non-text data over websockets :grimace:
#[derive(Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum RequestMessage {
    Start(StartMessage),
    Stop,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Data(model::Output),
    Active,
    Inactive,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub struct ResponseMessage {
    channel: usize,
    start_time: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_time: Option<f32>,
    data: Event,
}
