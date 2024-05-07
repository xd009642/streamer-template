use crate::StreamingContext;
use actix::{Actor, StreamHandler};
use actix_web::{web, App, Error, HttpRequest, HttpResponse, HttpServer};
use actix_web_actors::ws;
use std::sync::Arc;

struct ActorWrapper(Arc<StreamingContext>);

impl Actor for ActorWrapper {
    type Context = ws::WebsocketContext<Self>;
}

/// Handler for ws::Message message
impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for ActorWrapper {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        match msg {
            Ok(ws::Message::Ping(msg)) => ctx.pong(&msg),
            Ok(ws::Message::Text(text)) => ctx.text(text),
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
    let resp = ws::start(ActorWrapper(srv.into_inner()), &req, stream);
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
