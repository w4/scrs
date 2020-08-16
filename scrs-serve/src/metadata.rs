use arc_swap::ArcSwap;
use serde::Serialize;
use std::sync::atomic::AtomicU64;

#[derive(Debug, Default)]
pub struct MetadataContainer {
    pub stream: ArcSwap<Option<StreamMetadata>>,
    pub track: ArcSwap<Option<TrackMetadata>>,
    pub listeners: AtomicU64,
}

impl serde::ser::Serialize for MetadataContainer {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut struc = serializer.serialize_struct("", 3)?;
        struc.serialize_field("stream", &**self.stream.load())?;
        struc.serialize_field("track", &**self.track.load())?;
        struc.serialize_field("listeners", &self.listeners)?;
        struc.end()
    }
}

#[derive(Debug, Default, Serialize)]
pub struct StreamMetadata {
    pub user_agent: String,
    pub content_type: String,
    pub name: String,
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
pub struct TrackMetadata {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
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
