use serde::Deserialize;

use std::net::SocketAddr;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub stream: StreamConfig,
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    pub listen_address: SocketAddr,
}

#[derive(Debug, Deserialize)]
pub struct StreamConfig {
    // pub mount_point: String,
    pub content_type: String,
    pub bitrate: usize,
    pub max_conns: usize,
    pub password: String,
    pub buffer_size: usize,
}
