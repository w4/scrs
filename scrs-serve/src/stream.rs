use bus_queue::flavors::arc_swap::{async_bounded, AsyncPublisher, AsyncSubscriber};
use bytes::Bytes;
use mime::Mime;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;
use ustr::ustr;

use crate::metadata::MetadataContainer;

pub struct Stream {
    publisher: Mutex<AsyncPublisher<Bytes>>,
    subscriber: AsyncSubscriber<Bytes>,
    metadata: MetadataContainer,
    #[allow(dead_code)] // unimplemented
    bitrate: usize,
    #[allow(dead_code)] // unimplemented
    max_conns: usize,
    #[allow(dead_code)] // unimplemented
    password: String,
    pub content_type: Mime,
    pub listen_uri: &'static str,
}

impl From<crate::config::StreamConfig> for Stream {
    fn from(config: crate::config::StreamConfig) -> Self {
        let (publisher, subscriber) = async_bounded::<Bytes>(config.buffer_size);

        let content_type: Mime = config.content_type.parse().unwrap();

        let listen_uri = match (content_type.type_(), content_type.subtype()) {
            (mime::AUDIO, mime::MPEG) => ustr("/listen.mp3").as_str(),
            (mime::AUDIO, mime::OGG) => ustr("/listen.ogg").as_str(),
            _ => panic!("unknown stream content type"),
        };

        Self {
            publisher: Mutex::new(publisher),
            subscriber,
            metadata: MetadataContainer::default(),
            bitrate: config.bitrate,
            max_conns: config.max_conns,
            password: config.password,
            content_type,
            listen_uri,
        }
    }
}

impl Stream {
    pub fn listen(&self) -> Listener<'_> {
        Listener::new(self)
    }

    pub fn try_broadcast(&self) -> Result<BroadcastHandle<'_>, tokio::sync::TryLockError> {
        Ok(BroadcastHandle {
            publisher: self.publisher.try_lock()?,
        })
    }

    pub fn metadata(&self) -> &MetadataContainer {
        &self.metadata
    }
}

pub struct BroadcastHandle<'a> {
    pub publisher: tokio::sync::MutexGuard<'a, AsyncPublisher<Bytes>>,
}

pub struct Listener<'a> {
    pub subscriber: AsyncSubscriber<Bytes>,
    listener_count: &'a AtomicU64,
}

impl Drop for Listener<'_> {
    fn drop(&mut self) {
        self.listener_count.fetch_sub(1, Ordering::Relaxed);
    }
}

impl<'a> Listener<'a> {
    pub fn new(stream: &'a Stream) -> Listener<'a> {
        stream.metadata.listeners.fetch_add(1, Ordering::Relaxed);

        Self {
            subscriber: stream.subscriber.clone(),
            listener_count: &stream.metadata.listeners,
        }
    }
}
