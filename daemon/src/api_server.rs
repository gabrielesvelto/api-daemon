/// Actix WebSocket and HTTP server
use crate::global_context::GlobalContext;
use crate::session::Session;
use actix::{Actor, Addr, AsyncContext, Handler, StreamHandler};
use actix_cors::Cors;
use actix_web::http::header;
use actix_web::middleware::Logger;
use actix_web::web::Bytes;
use actix_web::{web, App, Error, HttpRequest, HttpResponse, HttpServer};
use actix_web_actors::ws;
use async_std::fs::File;
use common::traits::{
    IdFactory, MessageEmitter, MessageKind, MessageSender, SendMessageError, Shared,
};
use futures_core::{
    future::Future,
    ready,
    stream::Stream,
    task::{Context, Poll},
};
use futures_util::io::AsyncReadExt;
use log::{debug, error};
use std::sync::RwLock;

#[derive(Clone)]
struct ActorSender {
    sender: Addr<WsHandler>,
}

impl MessageEmitter for ActorSender {
    /// Sends a raw message
    fn send_raw_message(&self, message: MessageKind) {
        if let Err(err) = self.sender.try_send(message) {
            error!("Failed to send message from ActorSender! err={:?}", err);
        }
    }

    fn close_session(&self) -> Result<(), SendMessageError> {
        self.sender
            .try_send(MessageKind::Close)
            .map_err(|e| e.into())
    }
}

/// Define our WS actor, keeping track of the session.
struct WsHandler {
    session: Session,
}

impl Actor for WsHandler {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        // Create an ActorSender with our address and use it to replace
        // the session sender.
        self.session
            .replace_sender(MessageSender::new(Box::new(ActorSender {
                sender: ctx.address(),
            })));
    }
}

// Handler for our messages.
impl Handler<MessageKind> for WsHandler {
    type Result = ();

    fn handle(&mut self, msg: MessageKind, ctx: &mut Self::Context) {
        match msg {
            MessageKind::Data(_, val) => ctx.binary(val),
            MessageKind::ChildDaemonCrash(name) => {
                error!("Child daemon `{}` died, closing ws connection", name);
                ctx.close(None);
            }
            MessageKind::Close => ctx.close(None),
        }
    }
}

/// Handler for ws::Message message
impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for WsHandler {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        match msg {
            Ok(ws::Message::Ping(msg)) => ctx.pong(&msg),
            Ok(ws::Message::Binary(bin)) => {
                // Relay the message to the session.
                self.session.on_message(&bin);
            }
            Ok(ws::Message::Close(_)) => {
                debug!("Close WS client message {:?}", msg);
                self.session.close();
                ctx.close(None);
            }
            _ => {
                error!("Unexpected WS client message {:?}", msg);
                self.session.close();
                ctx.close(None);
            }
        }
    }
}

#[derive(Clone)]
pub struct SharedWsData {
    pub global_context: GlobalContext,
    session_id_factory: Shared<IdFactory>,
}

// A dummy message sender used when we initially create the session.
// It is replaced by the real one once the actor starts.
#[derive(Clone)]
struct DummySender {}

impl MessageEmitter for DummySender {
    fn send_raw_message(&self, _message: MessageKind) {}
    fn close_session(&self) -> Result<(), SendMessageError> {
        Ok(())
    }
}

// Starts a WS session.
async fn ws_index(
    data: web::Data<RwLock<SharedWsData>>,
    req: HttpRequest,
    stream: web::Payload,
) -> Result<HttpResponse, Error> {
    let data = data
        .read()
        .map_err(|_| HttpResponse::InternalServerError().finish())?;

    let global_context = &data.global_context;

    let session = Session::open(
        data.session_id_factory.lock().next_id() as u32,
        &global_context.config,
        MessageSender::new(Box::new(DummySender {})),
        global_context.tokens_manager.clone(),
        global_context.session_context.clone(),
        global_context.remote_service_manager.clone(),
        global_context.service_state(),
    );

    let resp = ws::start(WsHandler { session }, &req, stream);
    resp
}

// Returns the File and whether this is the gzip version.
async fn open_file(path: &str, gzip: bool) -> Result<(File, bool), ::std::io::Error> {
    // First test if we have a gzipped version.
    if gzip {
        let file = File::open(path.to_owned() + ".gz").await;
        if file.is_ok() {
            return Ok((file.unwrap(), true));
        }
    }

    File::open(path).await.map(|file| (file, false))
}

const CHUNK_SIZE: usize = 16 * 1024;

struct ChunkedFile {
    reader: File, //BufReader<File>,
}

impl ChunkedFile {
    fn new(file: File) -> Self {
        Self {
            reader: file, //BufReader::with_capacity(CHUNK_SIZE, file),
        }
    }
}

use std::pin::Pin;
impl Stream for ChunkedFile {
    type Item = Result<Bytes, ()>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        let mut buffer: [u8; CHUNK_SIZE] = [0; CHUNK_SIZE];

        let read = ready!(Future::poll(
            Pin::new(&mut self.reader.read(&mut buffer)),
            cx
        ))
        .map_err(|_| ())?;

        if read == 0 {
            // We reached EOF
            Poll::Ready(None)
        } else {
            Poll::Ready(Some(Self::Item::Ok(Bytes::copy_from_slice(
                &buffer[0..read],
            ))))
        }
    }
}

async fn http_index(
    data: web::Data<RwLock<SharedWsData>>,
    req: HttpRequest,
) -> Result<HttpResponse, Error> {
    let data = data
        .read()
        .map_err(|_| HttpResponse::InternalServerError().finish())?;

    let mut full_path = data.global_context.config.http.root_path.clone();
    full_path.push_str(req.path());

    let gzip_support = match req
        .headers()
        .get(::actix_web::http::header::ACCEPT_ENCODING)
    {
        Some(header_value) => match header_value.to_str() {
            Ok(value) => {
                let values: Vec<_> = value.split(',').map(|e| e.trim()).collect();
                values
                    .into_iter()
                    .find(|encoding| *encoding == "gzip")
                    .is_some()
            }
            Err(_) => false,
        },
        None => false,
    };

    match open_file(&full_path, gzip_support).await {
        Ok((mut file, gzipped)) => {
            // Send the file as a byte stream.
            let content_length = file.metadata().await?.len();
            let content_type = mime_guess::from_path(req.path()).first_or_octet_stream();

            let mut ok = HttpResponse::Ok();
            let builder = ok
                .if_true(gzipped, |builder| {
                    builder.header(header::CONTENT_ENCODING, "gzip");
                })
                .header(header::CONTENT_LENGTH, content_length)
                .header(header::CONTENT_TYPE, content_type);

            let response = if content_length <= CHUNK_SIZE as _ {
                // If the file is small enough, read it all and send it as body.
                let mut content = Vec::with_capacity(CHUNK_SIZE);
                file.read_to_end(&mut content).await?;
                builder.body(Bytes::from(content))
            } else {
                // Otherwise wrap the file in a chunked stream.
                builder.streaming(ChunkedFile::new(file))
            };

            Ok(response)
        }
        Err(_) => Ok(HttpResponse::NotFound().finish()),
    }
}

pub fn start(global_context: &GlobalContext) {
    let config = global_context.config.clone();
    let addr = format!("{}:{}", config.general.host, config.general.port);

    let sys = actix_rt::System::new("ws-server");
    let shared_data = SharedWsData {
        global_context: global_context.clone(),
        session_id_factory: Shared::adopt(IdFactory::new(0)),
    };

    HttpServer::new(move || {
        App::new()
            .wrap(Logger::new("%a \"%r\" %s %b %D"))
            .data(RwLock::new(shared_data.clone()))
            .wrap(
                Cors::new()
                    .send_wildcard()
                    .finish(),
            )
            .route("*", web::post().to(|| HttpResponse::MethodNotAllowed()))
            .route("/", web::get().to(ws_index))
            .route("/*", web::get().to(http_index))
    })
    .bind(addr)
    .expect("Failed to bind to actix ws")
    .disable_signals() // For now, since that's causing us issues with Ctrl-C
    .run();

    let _ = sys.run();
}

#[cfg(test)]
mod test {
    use crate::api_server;
    use crate::config::Config;
    use crate::global_context::GlobalContext;
    use reqwest::header::{CONTENT_ENCODING, CONTENT_TYPE};
    use reqwest::StatusCode;
    use std::{thread, time};

    fn start_server(port: u16) {
        // Create a new ws server.
        thread::spawn(move || {
            api_server::start(&GlobalContext::new(&Config::test_on_port(port)));
        });

        // Wait for the server to start.
        thread::sleep(time::Duration::from_millis(1000));
    }

    #[test]
    fn test_http_post_request() {
        start_server(9087);

        // Check that POST requests return a BadRequest status
        let client = reqwest::blocking::Client::new();
        let resp = client.post("http://localhost:9087/test").send().unwrap();

        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[test]
    fn test_not_found_http_get_request() {
        start_server(9088);

        // Check that GET requests return a NotFound status
        let resp = reqwest::blocking::get("http://localhost:9088/test/not_here").unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_valid_http_get_request() {
        start_server(9089);

        // Check that GET requests return a ok status
        let resp = reqwest::blocking::get("http://localhost:9089/core/index.js").unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        {
            let content_type = resp.headers().get(CONTENT_TYPE).unwrap();
            assert_eq!(content_type.as_bytes(), b"application/javascript");
        }
        dbg!(resp.headers());
        assert_eq!(resp.headers()["content-length"], "21");
        assert_eq!(resp.text().unwrap(), r#"console.log("Test!");"#);
    }

    #[test]
    fn test_octet_stream_http_get_request() {
        start_server(9090);

        // Check that GET requests return a ok status
        let resp = reqwest::blocking::get("http://localhost:9090/core/data.dat").unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        {
            let content_type = resp.headers().get(CONTENT_TYPE).unwrap();
            assert_eq!(content_type.as_bytes(), b"application/octet-stream");
        }

        assert_eq!(resp.headers()["content-length"], "0");
    }

    #[test]
    fn test_gzip_http_get_request() {
        start_server(9091);

        // Check that GET requests return a ok status with a gzip ContentEncoding
        let client = reqwest::blocking::Client::builder().build().unwrap();

        let resp = client
            .get("http://localhost:9091/core/data.dat")
            .header("Accept-Encoding", "gzip")
            .send()
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let headers = resp.headers();
        {
            let content_type = headers.get(CONTENT_TYPE).unwrap();
            assert_eq!(content_type.as_bytes(), b"application/octet-stream");
        }
        {
            let mut content_encodings = headers
                .get(CONTENT_ENCODING)
                .unwrap()
                .to_str()
                .unwrap()
                .split(',');
            assert!(content_encodings.any(|e| e == "gzip"));
        }

        assert_eq!(resp.headers()["content-length"], "29");
    }
}
