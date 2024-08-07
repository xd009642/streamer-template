use crate::{model, OutputEvent};
use opentelemetry::propagation::Extractor;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StartMessage {
    pub trace_id: Option<String>,
    pub channels: usize,
    pub sample_rate: usize,
}

impl Extractor for StartMessage {
    fn get(&self, key: &str) -> Option<&str> {
        if key == "traceparent" {
            self.trace_id.as_deref()
        } else {
            None
        }
    }

    fn keys(&self) -> Vec<&str> {
        vec!["traceparent"]
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StopMessage {
    pub disconnect: bool,
}

// TODO we might want to do a base64 message here, I dislike the APIs but some people don't realise
// you can send non-text data over websockets :grimace:
#[derive(Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum RequestMessage {
    Start(StartMessage),
    Stop(StopMessage),
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Data(model::Output),
    Error(String),
    Active,
    Inactive,
}

impl From<OutputEvent> for Event {
    fn from(event: OutputEvent) -> Self {
        match event {
            OutputEvent::Response(o) => Event::Data(o),
            OutputEvent::ModelError(e) => Event::Error(e),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub struct ResponseMessage {
    pub channel: usize,
    pub start_time: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<f32>,
    pub data: Event,
}
