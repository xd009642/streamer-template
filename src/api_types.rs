use crate::model;
use opentelemetry::propagation::Extractor;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StartMessage {
    /// Trace ID for distributed tracing
    pub trace_id: Option<String>,
    /// Format information for the audio samples
    pub format: AudioFormat,
    /// Whether interim results should be provided. An alternative API to this would be to specify
    /// the interval at which interim results are returned.
    #[serde(default)]
    pub interim_results: bool,
    // TODO here we likely need some configuration to let people do things like configure the VAD
    // sensitivity.
}

/// Describes the PCM samples coming in. I could have gone for an enum instead of bit_depth +
/// is_float but I only really plan on doing f32, s16 and ignoring everything else including
/// compression schemes like mulaw etc. This may change in future.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AudioFormat {
    /// Number of channels in the audio
    pub channels: usize,
    /// Sample rate of the audio
    pub sample_rate: usize,
    /// Number of bits per sample
    pub bit_depth: u16,
    /// Whether audio uses floating point samples
    pub is_float: bool,
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
    /// Whether the server should close the websocket connection after returning final results.
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

/// If we're processing segments of audio we want people to be able to apply the results to the
/// actual segments!
#[derive(Debug, Serialize, Deserialize)]
pub struct SegmentOutput {
    /// Start time of the segment in seconds
    pub start_time: f32,
    /// End time of the segment in seconds
    pub end_time: f32,
    /// Some APIs may do the inverse check of "is_partial" where the last request in an utterance
    /// would be `false`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_final: Option<bool>,
    /// The output from our ML model
    #[serde(flatten)]
    pub output: model::Output,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Data(model::Output),
    Segment(SegmentOutput),
    Error { message: String },
    Active { time: f32 },
    Inactive { time: f32 },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ApiResponse {
    #[serde(flatten)]
    pub data: Event,
    pub channel: usize,
}
