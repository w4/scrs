use crate::metadata::{StreamMetadata, TrackMetadata};

use derive_more::Deref;
use futures::{SinkExt, StreamExt};

use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use std::convert::TryFrom;
use std::sync::Arc;

#[derive(Error, Debug)]
pub enum HandlerError {
    #[error("failed to parse request from client: `{0}`")]
    RequestError(#[from] httparse::Error),
    #[error("failed to create response for client: `{0}`")]
    ResponseError(#[from] http::Error),
    #[error("other side didn't send valid a valid http request")]
    InvalidHttp,
    #[error("failed to create JSON blob response for client: `{0}`")]
    JsonWriteError(serde_json::Error),
    #[error("i/o failed for client: `{0}`")]
    IoError(#[from] std::io::Error),
    #[error("source attempted to stream data, but the stream no longer exists: `{0}`")]
    StreamDisconnected(#[from] std::sync::mpsc::SendError<bytes::Bytes>),
}

#[derive(Debug, Deref)]
struct Request(http::Request<()>);

impl TryFrom<httparse::Request<'_, '_>> for Request {
    type Error = HandlerError;

    fn try_from(parsed: httparse::Request<'_, '_>) -> Result<Self, Self::Error> {
        let mut req = http::Request::builder()
            .version(http::Version::HTTP_11)
            .method(parsed.method.ok_or(HandlerError::InvalidHttp)?)
            .uri(parsed.path.ok_or(HandlerError::InvalidHttp)?);

        for header in parsed.headers {
            req.headers_mut().unwrap().insert(
                http::header::HeaderName::from_bytes(header.name.as_bytes())
                    .map_err(|_| httparse::Error::HeaderName)?,
                http::HeaderValue::try_from(header.value)
                    .map_err(|_| httparse::Error::HeaderValue)?,
            );
        }

        Ok(Self(req.body(())?))
    }
}

macro_rules! write_response {
    ($writer:ident, $resp:expr) => {{
        let resp = $resp.body(())?;

        $writer
            .write_all(
                format!(
                    "{:?} {} {}\r\n",
                    resp.version(),
                    resp.status().as_str(),
                    resp.status().canonical_reason().unwrap_or("unknown error")
                )
                .as_bytes(),
            )
            .await?;

        for (name, value) in resp.headers() {
            $writer.write_all(name.as_str().as_bytes()).await?;
            $writer.write_all(b": ").await?;
            $writer.write_all(value.as_bytes()).await?;
            $writer.write_all(b"\r\n").await?;
        }

        $writer.write_all(b"\r\n").await?;
        $writer.flush().await?;
    }};
}

macro_rules! write_json {
    ($conn:ident, $x:expr) => {
        $conn
            .write_all(
                serde_json::to_string($x)
                    .map_err(HandlerError::JsonWriteError)?
                    .as_bytes(),
            )
            .await?;
    };
}

fn header_bytes_or_empty<'a>(req: &'a Request, header: &str) -> &'a [u8] {
    req.headers()
        .get(header)
        .map(http::HeaderValue::as_bytes)
        .unwrap_or_default()
}

pub async fn process(
    mut conn: TcpStream,
    stream: Arc<crate::stream::Stream>,
) -> Result<(), HandlerError> {
    let mut buffer = bytes::BytesMut::with_capacity(1024);

    if conn.read_buf(&mut buffer).await? == 0 {
        return Ok(());
    }

    let req = {
        let mut headers = [httparse::EMPTY_HEADER; 16];
        let mut parsed = httparse::Request::new(&mut headers);

        let header_buffer = buffer.split().freeze();
        parsed.parse(&header_buffer[..])?;

        Request::try_from(parsed)?
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
        v if v == stream.listen_uri => {
            // TODO: allow the streamer to set content-type
            let resp = resp.header("Content-Type", stream.content_type.as_ref());
            write_response!(conn, resp);

            let mut handle = stream.listen();

            loop {
                if let Some(v) = handle.subscriber.next().await {
                    conn.write_all(v.as_ref()).await?;
                }
            }
        }
        "/stream" => {
            println!("stream req: {:?}", req.headers());
            if let Ok(ref mut handle) = stream.try_broadcast() {
                let mut resp = resp
                    .header("Connection", "Close")
                    .header("Allow", "PUT, SOURCE");

                let content_type = header_bytes_or_empty(&req, "content-type");
                if content_type != stream.content_type.as_ref().as_bytes() {
                    // todo: custom message - unsupported content type
                    write_response!(conn, resp.status(403));
                    return Ok(());
                }

                let expect_header = header_bytes_or_empty(&req, "expect");
                if expect_header == b"100-continue" {
                    resp = resp.status(100);
                }

                write_response!(conn, resp);

                // todo: only stream handles should have write access to the metadata
                stream
                    .metadata()
                    .stream
                    .store(Arc::new(Some(StreamMetadata::from(req.headers()))));

                loop {
                    if conn.read_buf(&mut buffer).await? == 0 {
                        break;
                    }

                    handle.publisher.send(buffer.split().freeze()).await?;
                }

                stream.metadata().stream.store(Arc::new(None));
                stream.metadata().track.store(Arc::new(None));
            } else {
                panic!("someone's already streaming!");
            }
        }
        "/metadata" => {
            let resp = resp
                .header("Connection", "Close")
                .header("Content-Type", "application/json");
            write_response!(conn, resp);
            write_json!(conn, stream.metadata());
        }
        "/admin/metadata" => {
            if let Some(query) = req
                .uri()
                .query()
                .map(str::as_bytes)
                .map(url::form_urlencoded::parse)
            {
                // todo: auth
                stream
                    .metadata()
                    .track
                    .store(Arc::new(Some(TrackMetadata::from(query))));

                write_response!(conn, resp);
            }
        }
        _ => panic!("Invalid request: {:?}", req),
    }

    Ok(())
}
