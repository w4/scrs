#![deny(clippy::pedantic)]
#![allow(clippy::used_underscore_binding)]

use arc_swap::ArcSwap;
use bus_queue::flavors::arc_swap::{bounded, Publisher, Subscriber};
use bytes::Bytes;
use derive_more::Deref;
use futures::{SinkExt, StreamExt};
use serde::Serialize;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use std::convert::TryFrom;
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Debug, Deref)]
struct Request(http::Request<()>);

impl From<httparse::Request<'_, '_>> for Request {
    fn from(parsed: httparse::Request<'_, '_>) -> Self {
        let mut req = http::Request::builder()
            .version(http::Version::HTTP_11)
            .method(parsed.method.unwrap())
            .uri(parsed.path.unwrap());

        for header in parsed.headers {
            req.headers_mut().unwrap().insert(
                http::header::HeaderName::from_bytes(header.name.as_bytes()).unwrap(),
                http::HeaderValue::try_from(header.value).unwrap(),
            );
        }

        Self(req.body(()).unwrap())
    }
}

#[derive(Debug, Default)]
struct MetadataContainer {
    stream: ArcSwap<Option<StreamMetadata>>,
    track: ArcSwap<Option<TrackMetadata>>,
}

impl serde::ser::Serialize for MetadataContainer {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut struc = serializer.serialize_struct("", 2)?;
        struc.serialize_field("stream", &**self.stream.load())?;
        struc.serialize_field("track", &**self.track.load())?;
        struc.end()
    }
}

#[derive(Debug, Default, Serialize)]
struct StreamMetadata {
    user_agent: String,
    content_type: String,
    name: String,
}

impl From<&http::HeaderMap> for StreamMetadata {
    fn from(map: &http::HeaderMap) -> Self {
        let string_from_header = |v: Option<&http::HeaderValue>| {
            v.map(http::HeaderValue::as_bytes)
                .map(String::from_utf8_lossy)
                .unwrap_or_default()
                .into_owned()
        };

        Self {
            user_agent: string_from_header(map.get("user-agent")),
            content_type: string_from_header(map.get("content-type")),
            name: string_from_header(map.get("ice-name")),
        }
    }
}

#[derive(Debug, Default, Serialize)]
struct TrackMetadata {
    artist: Option<String>,
    title: Option<String>,
    album: Option<String>,
}

impl From<url::form_urlencoded::Parse<'_>> for TrackMetadata {
    fn from(query: url::form_urlencoded::Parse<'_>) -> Self {
        let mut meta = Self::default();

        for (key, value) in query {
            match key.as_ref() {
                "artist" => meta.artist = Some(value.into_owned()),
                "title" => meta.title = Some(value.into_owned()),
                "album" => meta.album = Some(value.into_owned()),
                _ => {}
            }
        }

        meta
    }
}

async fn write_response<W: tokio::io::AsyncWrite + Unpin>(mut writer: W, resp: http::Response<()>) {
    writer
        .write_all(
            format!(
                "{:?} {} {}\r\n",
                resp.version(),
                resp.status().as_str(),
                resp.status().canonical_reason().unwrap()
            )
            .as_bytes(),
        )
        .await
        .unwrap();

    for (name, value) in resp.headers() {
        writer.write_all(name.as_str().as_bytes()).await.unwrap();
        writer.write_all(b": ").await.unwrap();
        writer.write_all(value.as_bytes()).await.unwrap();
        writer.write_all(b"\r\n").await.unwrap();
    }

    writer.write_all(b"\r\n").await.unwrap();
    writer.flush().await.unwrap();
}

async fn process(
    mut stream: TcpStream,
    publisher: Arc<Mutex<Publisher<Bytes>>>,
    mut subscriber: Subscriber<Bytes>,
    metadata: Arc<MetadataContainer>,
) {
    println!("accepted");

    let mut buffer = bytes::BytesMut::with_capacity(1024);

    if stream.read_buf(&mut buffer).await.unwrap() == 0 {
        return;
    }

    let req = {
        let mut headers = [httparse::EMPTY_HEADER; 16];
        let mut parsed = httparse::Request::new(&mut headers);

        let header_buffer = buffer.split().freeze();
        parsed.parse(&header_buffer[..]).unwrap();

        Request::from(parsed)
    };

    let resp = http::Response::builder()
        .version(http::Version::HTTP_11)
        .header(
            "Server",
            concat!(clap::crate_name!(), "/", clap::crate_version!()),
        )
        .header("Accept-Encoding", "identity")
        .header("Connection", "keep-alive")
        .header("Access-Control-Allow-Origin", "*");

    match req.uri().path() {
        "/listen.mp3" => {
            // TODO: allow the streamer to set content-type
            let resp = resp.header("Content-Type", "audio/mpeg").body(()).unwrap();
            write_response(&mut stream, resp).await;

            loop {
                if let Some(v) = subscriber.next().await {
                    if stream.write_all(v.as_ref()).await.is_err() {
                        break;
                    }
                }
            }
        }
        "/stream" => {
            println!("stream req: {:?}", req.headers());
            if let Ok(ref mut publisher) = publisher.try_lock() {
                metadata
                    .stream
                    .store(Arc::new(Some(StreamMetadata::from(req.headers()))));

                let mut resp = resp
                    .header("Connection", "Close")
                    .header("Allow", "PUT, SOURCE");

                let expect_header = req
                    .headers()
                    .get("expect")
                    .map(http::HeaderValue::as_bytes)
                    .unwrap_or_default();
                if expect_header == b"100-continue" {
                    resp = resp.status(100);
                }

                write_response(&mut stream, resp.body(()).unwrap()).await;

                loop {
                    if stream.read_buf(&mut buffer).await.unwrap() == 0 {
                        break;
                    }

                    publisher.send(buffer.split().freeze()).await.unwrap();
                }

                metadata.stream.store(Arc::new(None));
                metadata.track.store(Arc::new(None));
            } else {
                panic!("someone's already streaming!");
            }
        }
        "/metadata" => {
            let resp = resp
                .header("Connection", "Close")
                .header("Content-Type", "application/json")
                .body(())
                .unwrap();
            write_response(&mut stream, resp).await;

            stream
                .write_all(serde_json::to_string(&*metadata).unwrap().as_bytes())
                .await
                .unwrap();
        }
        "/admin/metadata" => {
            let query = url::form_urlencoded::parse(req.uri().query().unwrap().as_bytes());

            metadata
                .track
                .store(Arc::new(Some(TrackMetadata::from(query))));

            let resp = resp.body(()).unwrap();
            write_response(&mut stream, resp).await;
        }
        _ => panic!("Invalid request: {:?}", req),
    }
}

#[tokio::main]
async fn main() {
    env_logger::init();
    listen_forward(3000).await;
}

async fn listen_forward(port: u16) {
    let (publisher, subscriber) = bounded::<bytes::Bytes>(128);
    let publisher = Arc::new(Mutex::new(publisher));
    let metadata = Arc::new(MetadataContainer::default());

    let addr = SocketAddr::from(([127, 0, 0, 1], port));

    // let the consumer pass this in
    let mut listener = tokio::net::TcpListener::bind(addr).await.unwrap();

    loop {
        println!("listening for new conns...");
        let (stream, _) = listener.accept().await.unwrap();

        let publisher = publisher.clone();
        let subscriber = subscriber.clone();
        let metadata = metadata.clone();

        tokio::spawn(process(stream, publisher, subscriber, metadata));
    }
}
