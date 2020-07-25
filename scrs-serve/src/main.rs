#![deny(clippy::pedantic)]
#![allow(clippy::used_underscore_binding)]
#![allow(clippy::module_name_repetitions)]

mod config;
mod handler;
mod metadata;
mod stream;

use clap::Clap;
use log::{debug, error, info};
use std::sync::Arc;
use tokio::net::TcpListener;

#[derive(thiserror::Error, Debug)]
pub enum InitError {
    #[error("Couldn't read config file: `{0}`")]
    ReadError(#[from] std::io::Error),
    #[error("Couldn't parse config file: `{0}`")]
    DeserializationError(#[from] toml::de::Error),
}

#[derive(Clap)]
#[clap(version = "1.0", author = "Jordan D. <jordan@doyle.la>")]
struct Opts {
    #[clap(short, long)]
    config: std::path::PathBuf,
}

#[tokio::main]
async fn main() {
    env_logger::init();

    let opts: Opts = Opts::parse();

    let cfg = match std::fs::read_to_string(opts.config)
        .map_err(InitError::ReadError)
        .and_then(|s| toml::from_str(&s).map_err(Into::into))
    {
        Ok(v) => v,
        Err(e) => {
            error!("{}", e);
            return;
        }
    };

    listen_forward(cfg).await;
}

async fn listen_forward(config: config::Config) {
    let stream = Arc::new(stream::Stream::from(config.stream));

    let mut listener = TcpListener::bind(config.server.listen_address)
        .await
        .unwrap();

    info!(
        "Listening for new connections on {}",
        config.server.listen_address
    );

    loop {
        let (conn, remote) = listener.accept().await.unwrap();
        debug!("Accepted connection from {}", remote);

        let stream = stream.clone();

        tokio::spawn(async move {
            if let Err(e) = handler::process(conn, stream).await {
                error!("Error handling request from {}: {}", remote, e);
            }
        });
    }
}
