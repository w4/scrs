use crate::metadata::{StreamMetadata, TrackMetadata};

use derive_more::Deref;
use futures::{SinkExt, StreamExt};
use slog::{debug, info};
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
    #[error("source is already connected to the requested mountpoint")]
    StreamAlreadyConnected,
    #[error("source attempted to stream mime-type {} whereas the stream is defined as {}", .actual, .expected)]
    UnsupportedContentType {
        actual: String,
        expected: mime::Mime,
    },
    #[error("endpoint not found: `{} {}`", .0.method(), .0.uri())]
    EndpointNotFound(http::Request<()>),
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
    ($log:ident, $writer:ident, $resp:expr) => {{
        write_response!($log, $writer, $resp, "");
    }};

    ($log:ident, $writer:ident, $resp:expr, $canonical_reason:expr) => {{
        let resp = $resp.body(())?;

        let resp_head = format!(
            "{:?} {} {}\r\n",
            resp.version(),
            resp.status().as_str(),
            if $canonical_reason == "" {
                resp.status().canonical_reason().unwrap_or("unknown error")
            } else {
                $canonical_reason
            }
        );
        debug!($log, "Response: {}", &resp_head[..resp_head.len() - 2]);

        $writer.write_all(resp_head.as_bytes()).await?;

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

#[allow(clippy::too_many_lines)]
pub async fn process(
    mut conn: TcpStream,
    stream: Arc<crate::stream::Stream>,
    log: slog::Logger,
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

    debug!(log, "Request: {} {} {:?}", req.method(), req.uri(), req.version();
        "user-agent" => req.headers().get("user-agent").and_then(|v| v.to_str().ok()));

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
            write_response!(log, conn, resp);

            let mut handle = stream.listen();
            debug!(log, "Streaming to new listener"; "mountpoint" => "/");

            loop {
                if let Some(v) = handle.subscriber.next().await {
                    conn.write_all(v.as_ref()).await?;
                }
            }
        }
        "/stream" => {
            if let Ok(ref mut handle) = stream.try_broadcast() {
                let mut resp = resp
                    .header("Connection", "Close")
                    .header("Allow", "PUT, SOURCE");

                let content_type = header_bytes_or_empty(&req, "content-type");
                if content_type != stream.content_type.as_ref().as_bytes() {
                    let canonical_reason = format!("Content-Type must be {}", stream.content_type);
                    write_response!(log, conn, resp.status(403), &canonical_reason);
                    return Err(HandlerError::UnsupportedContentType {
                        actual: String::from_utf8_lossy(content_type).into_owned(),
                        expected: stream.content_type.clone(),
                    });
                }

                let expect_header = header_bytes_or_empty(&req, "expect");
                if expect_header == b"100-continue" {
                    resp = resp.status(100);
                }

                write_response!(log, conn, resp);

                let metadata = StreamMetadata::from(req.headers());
                info!(log, "Accepted stream request";
                    "mountpoint" => "/",
                    "user-agent" => &metadata.user_agent,
                    "content-type" => &metadata.content_type,
                    "name" => &metadata.name);

                // todo: only stream handles should have write access to the metadata
                stream.metadata().stream.store(Arc::new(Some(metadata)));

                loop {
                    if conn.read_buf(&mut buffer).await? == 0 {
                        break;
                    }

                    handle.publisher.send(buffer.split().freeze()).await?;
                }

                stream.metadata().stream.store(Arc::new(None));
                stream.metadata().track.store(Arc::new(None));
            } else {
                write_response!(log, conn, resp.status(403), "Mountpoint in use");
                return Err(HandlerError::StreamAlreadyConnected);
            }
        }
        "/metadata" => {
            let resp = resp
                .header("Connection", "Close")
                .header("Content-Type", "application/json");
            write_response!(log, conn, resp);
            write_json!(conn, stream.metadata());
        }
        "/admin/metadata" => {
            if let Some(query) = req
                .uri()
                .query()
                .map(str::as_bytes)
                .map(url::form_urlencoded::parse)
            {
                let metadata = TrackMetadata::from(query);

                debug!(log, "Updated stream metadata";
                    "mountpoint" => "/",
                    "track" => metadata.title,
                    "artist" => metadata.artist,
                    "album" => metadata.album);

                // todo: auth
                stream
                    .metadata()
                    .track
                    .store(Arc::new(Some(TrackMetadata::from(query))));

                write_response!(log, conn, resp);
            }
        }
        _ => {
            let resp = resp.header("Connection", "Close").status(404);
            write_response!(log, conn, resp);

            return Err(HandlerError::EndpointNotFound(req.0));
        }
    }

    Ok(())
}
