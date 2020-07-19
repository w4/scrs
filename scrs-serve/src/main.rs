use bus_queue::flavors::arc_swap::{bounded, Publisher, Subscriber};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use tokio::sync::Mutex;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use std::convert::TryFrom;
use std::net::SocketAddr;
use std::sync::Arc;

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

        let mut req = http::Request::builder()
            .version(http::Version::HTTP_11)
            .method(parsed.method.unwrap())
            .uri(parsed.path.unwrap());
        for header in parsed.headers.as_ref() {
            println!("parsing: {}", header.name);
            req.headers_mut().unwrap().insert(
                http::header::HeaderName::from_bytes(header.name.as_bytes()).unwrap(),
                http::HeaderValue::try_from(header.value).unwrap(),
            );
        }
        req.body(()).unwrap()
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
                    if let Err(_) = stream.write_all(v.as_ref()).await {
                        break;
                    }
                }
            }
        }
        "/stream" => {
            println!("stream req: {:?}", req.headers());
            if let Ok(ref mut publisher) = publisher.try_lock() {
                let resp = resp
                    .header("Connection", "Close")
                    .header("Allow", "PUT, SOURCE")
                    .body(())
                    .unwrap();
                write_response(&mut stream, resp).await;

                loop {
                    if stream.read_buf(&mut buffer).await.unwrap() == 0 {
                        break;
                    }

                    publisher.send(buffer.split().freeze()).await.unwrap();
                }
            } else {
                panic!("someone's already streaming!");
            }
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

    let addr = SocketAddr::from(([127, 0, 0, 1], port));

    // let the consumer pass this in
    let mut listener = tokio::net::TcpListener::bind(addr).await.unwrap();

    loop {
        println!("listening for new conns...");
        let (stream, _) = listener.accept().await.unwrap();

        let publisher = publisher.clone();
        let subscriber = subscriber.clone();

        tokio::spawn(process(stream, publisher, subscriber));
    }
}
