use crate::api_types::*;
use crate::AudioChannel;
use crate::audio::decode_audio;
use crate::{OutputEvent, StreamingContext};
use actix::{Actor, StreamHandler};
use actix_web::{web, App, Error, HttpRequest, HttpResponse, HttpServer};
use actix_web_actors::ws;
use bytes::Bytes;
use std::sync::Arc;
use tokio::sync::mpsc;

type StreamerInput = mpsc::Sender<AudioChannel>;
type StreamerOutput = mpsc::Receiver<OutputEvent>;

#[derive(Default)]
struct WebsocketState {
    audio_input: Option<mpsc::Sender<Bytes>>,
    inference_handles: Vec<>,
    streamer_inputs: Vec<StreamerInput>,
    returned_outputs: Vec<StreamerOutput>,
    streamer: Arc<StreamingContext>,
    format: Option<StartMessage>,
}

impl WebsocketState {
    fn start_channel_tasks(&mut self, msg: StartMessage) {
        // Create channels then spawn a task for the transcoding
        let (tx, rx) = mpsc::channel(16);
        let audio_fut = decode_audio(msg.sample_rate, rx, 
        self.format = Some(msg);
    }
}

impl From<Arc<StreamingContext>> for WebsocketState {
    fn from(streamer: Arc<StreamingContext>) -> Self {
        Self {
            streamer,
            ..Default::default()
        }
    }
}

impl Actor for WebsocketState {
    type Context = ws::WebsocketContext<Self>;
}

/// Handler for ws::Message message
impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for WebsocketState {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        match msg {
            Ok(ws::Message::Ping(msg)) => ctx.pong(&msg),
            Ok(ws::Message::Text(text)) => {
                match serde_json::from_str::<RequestMessage>(&text) {
                    Ok(RequestMessage::Start(start)) => {
                        if self.format.is_some() {
                            // Resetting the start format without a stop we're going to say isn't
                            // allowed. But if the start is the same as the previous start we can
                            // just ignore it as a no-op and try to be nice
                            match &self.format {
                                Some(existing_start) if *existing_start == start => {}
                                Some(s) => {}
                                None => {
                                    self.start_channel_tasks(start);
                                }
                            }
                        }
                    }
                    Ok(RequestMessage::Stop) => {
                        // Okay we want to stop what's going into our streaming context
                    }
                    Err(e) => {}
                }
                ctx.text(text)
            }
            Ok(ws::Message::Binary(bin)) => ctx.binary(bin),
            _ => (),
        }
    }
}

async fn audio_stream(
    req: HttpRequest,
    stream: web::Payload,
    srv: web::Data<StreamingContext>,
) -> Result<HttpResponse, Error> {
    let resp = ws::start(WebsocketState::from(srv.into_inner()), &req, stream);
    println!("{:?}", resp);
    resp
}

#[actix_web::main]
pub async fn run_actix_server(app_state: Arc<StreamingContext>) -> std::io::Result<()> {
    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::from(app_state.clone()))
            .route("/api/v1/stream", web::get().to(audio_stream))
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}
