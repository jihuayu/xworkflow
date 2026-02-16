//! Execution IO value types shared across layers.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::collections::HashMap;
use std::ops::{Deref, Index};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use tokio::sync::{Notify, RwLock};

use crate::domain::model::SegmentType;

// ================================
// Segment – Dify variable type system
// ================================

/// A dynamically-typed variable value used throughout the workflow engine.
///
/// `Segment` mirrors the Dify variable type system and supports:
/// - Primitive types: `String`, `Integer`, `Float`, `Boolean`
/// - Composite types: `Object` (key-value map), `Array`, `ArrayString`
/// - `Stream` for LLM streaming responses
/// - `None` for null / missing values
#[derive(Debug, Clone)]
pub enum Segment {
    None,
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Object(Arc<SegmentObject>),
    ArrayString(Arc<Vec<String>>),
    Array(Arc<SegmentArray>),
    Stream(SegmentStream),
    File(Arc<FileSegment>),
    ArrayFile(Arc<Vec<FileSegment>>),
}

/// A heterogeneous array of [`Segment`] values with a lazily-cached JSON
/// representation.
#[derive(Debug, Default)]
pub struct SegmentArray {
    items: Vec<Segment>,
    cached_value: OnceLock<Value>,
}

impl SegmentArray {
    /// Create a new `SegmentArray` from a vector of segments.
    pub fn new(items: Vec<Segment>) -> Self {
        Self {
            items,
            cached_value: OnceLock::new(),
        }
    }

    pub(crate) fn to_value(&self) -> Value {
        self.cached_value
            .get_or_init(|| Value::Array(self.items.iter().map(|s| s.to_value()).collect()))
            .clone()
    }

    pub(crate) fn into_value(self) -> Value {
        if let Some(v) = self.cached_value.into_inner() {
            return v;
        }
        Value::Array(self.items.into_iter().map(|s| s.into_value()).collect())
    }

    pub(crate) fn push(&mut self, value: Segment) {
        self.items.push(value);
        self.cached_value = OnceLock::new();
    }
}

impl Clone for SegmentArray {
    fn clone(&self) -> Self {
        Self {
            items: self.items.clone(),
            cached_value: OnceLock::new(),
        }
    }
}

impl Deref for SegmentArray {
    type Target = Vec<Segment>;

    fn deref(&self) -> &Self::Target {
        &self.items
    }
}

/// A string-keyed map of [`Segment`] values with a lazily-cached JSON
/// representation.
#[derive(Debug, Default)]
pub struct SegmentObject {
    entries: HashMap<String, Segment>,
    cached_value: OnceLock<Value>,
}

impl SegmentObject {
    /// Create a new `SegmentObject` from a `HashMap`.
    pub fn new(entries: HashMap<String, Segment>) -> Self {
        Self {
            entries,
            cached_value: OnceLock::new(),
        }
    }

    pub(crate) fn to_value(&self) -> Value {
        self.cached_value
            .get_or_init(|| {
                let m: serde_json::Map<std::string::String, Value> = self
                    .entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_value()))
                    .collect();
                Value::Object(m)
            })
            .clone()
    }

    pub(crate) fn into_value(self) -> Value {
        if let Some(v) = self.cached_value.into_inner() {
            return v;
        }
        Value::Object(
            self.entries
                .into_iter()
                .map(|(k, v)| (k, v.into_value()))
                .collect(),
        )
    }
}

impl Clone for SegmentObject {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            cached_value: OnceLock::new(),
        }
    }
}

impl Deref for SegmentObject {
    type Target = HashMap<String, Segment>;

    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

/// Serializable file reference used by File-type variables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FileTransferMethod {
    #[default]
    RemoteUrl,
    LocalFile,
    ToolFile,
    InternalStorage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileCategory {
    Image,
    Document,
    Audio,
    Video,
    Other,
}

fn default_mime_type() -> String {
    "application/octet-stream".to_string()
}

/// Serializable file reference used by File-type variables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSegment {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default = "default_mime_type")]
    pub mime_type: String,
    #[serde(default)]
    pub transfer_method: FileTransferMethod,

    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Map<String, Value>>,
}

impl Default for FileSegment {
    fn default() -> Self {
        Self {
            name: String::new(),
            size: 0,
            mime_type: default_mime_type(),
            transfer_method: FileTransferMethod::RemoteUrl,
            id: None,
            url: None,
            extension: None,
            last_modified: None,
            hash: None,
            extra: None,
        }
    }
}

impl FileSegment {
    /// Convert this file metadata into a [`Segment::File`].
    pub fn to_segment(&self) -> Segment {
        Segment::File(Arc::new(self.clone()))
    }

    /// Try to reconstruct a `FileSegment` from a [`Segment`] snapshot.
    pub fn from_segment(seg: &Segment) -> Option<Self> {
        match seg {
            Segment::File(file) => Some(file.as_ref().clone()),
            _ => None,
        }
    }

    pub fn from_url(url: String, name: String, mime_type: String, size: u64) -> Self {
        Self {
            name,
            size,
            mime_type,
            transfer_method: FileTransferMethod::RemoteUrl,
            url: Some(url),
            extension: None,
            id: None,
            last_modified: None,
            hash: None,
            extra: None,
        }
    }

    pub fn from_local_path(path: String, name: String, mime_type: String, size: u64) -> Self {
        Self {
            name,
            size,
            mime_type,
            transfer_method: FileTransferMethod::LocalFile,
            id: Some(path),
            url: None,
            extension: None,
            last_modified: None,
            hash: None,
            extra: None,
        }
    }

    pub fn from_storage_id(id: String, name: String, mime_type: String, size: u64) -> Self {
        Self {
            name,
            size,
            mime_type,
            transfer_method: FileTransferMethod::InternalStorage,
            id: Some(id),
            url: None,
            extension: None,
            last_modified: None,
            hash: None,
            extra: None,
        }
    }

    pub fn extension(&self) -> Option<&str> {
        self.extension.as_deref().or_else(|| {
            Path::new(self.name.as_str())
                .extension()
                .and_then(|s| s.to_str())
        })
    }

    pub fn file_category(&self) -> FileCategory {
        let mime = self.mime_type.to_lowercase();
        if mime.starts_with("image/") {
            return FileCategory::Image;
        }
        if mime.starts_with("audio/") {
            return FileCategory::Audio;
        }
        if mime.starts_with("video/") {
            return FileCategory::Video;
        }
        if mime.starts_with("text/")
            || mime == "application/pdf"
            || mime.starts_with("application/vnd.")
            || mime.starts_with("application/msword")
        {
            return FileCategory::Document;
        }
        FileCategory::Other
    }
}

// ================================
// Stream support
// ================================

/// An event emitted by a [`SegmentStream`] writer.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A new chunk of data.
    Chunk(Segment),
    /// The stream has completed with a final aggregated value.
    End(Segment),
    /// The stream encountered an error.
    Error(String),
}

/// Current status of a [`SegmentStream`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone)]
struct StreamState {
    chunks: Arc<SegmentArray>,
    buffer_bytes: usize,
    limits: StreamLimits,
    status: StreamStatus,
    final_value: Option<Segment>,
    error: Option<String>,
}

/// An async stream of [`Segment`] chunks used for LLM streaming responses.
///
/// Created via [`SegmentStream::channel`]. The writer end ([`StreamWriter`])
/// pushes chunks; readers ([`StreamReader`]) consume them asynchronously.
#[derive(Debug, Clone)]
pub struct SegmentStream {
    state: Arc<RwLock<StreamState>>,
    notify: Arc<Notify>,
    readers: Arc<AtomicUsize>,
}

/// Write end of a [`SegmentStream`] channel. Use [`StreamWriter::send`] to
/// push chunks and [`StreamWriter::end`] to finalize.
#[derive(Debug)]
pub struct StreamWriter {
    state: Arc<RwLock<StreamState>>,
    notify: Arc<Notify>,
    readers: Arc<AtomicUsize>,
    writers: Arc<AtomicUsize>,
}

/// Read end of a [`SegmentStream`]. Tracks a cursor position to iterate
/// through chunks as they arrive.
#[derive(Debug)]
pub struct StreamReader {
    stream: SegmentStream,
    cursor: usize,
}

#[derive(Clone, Debug)]
#[doc(hidden)]
pub struct StreamStateWeak {
    inner: Weak<RwLock<StreamState>>,
}

impl StreamStateWeak {
    pub fn is_dropped(&self) -> bool {
        self.inner.upgrade().is_none()
    }

    pub fn strong_count(&self) -> usize {
        self.inner.strong_count()
    }
}

/// Resource limits for a stream channel.
#[derive(Debug, Clone, Default)]
pub struct StreamLimits {
    /// Maximum number of chunks before the stream is auto-ended.
    pub max_chunks: Option<usize>,
    /// Maximum cumulative buffer size in bytes.
    pub max_buffer_bytes: Option<usize>,
}

impl SegmentStream {
    /// Create a new stream channel with default (unlimited) limits.
    pub fn channel() -> (SegmentStream, StreamWriter) {
        Self::channel_with_limits(StreamLimits::default())
    }

    /// Create a new stream channel with the specified resource limits.
    pub fn channel_with_limits(limits: StreamLimits) -> (SegmentStream, StreamWriter) {
        let state = StreamState {
            chunks: Arc::new(SegmentArray::new(Vec::new())),
            buffer_bytes: 0,
            limits,
            status: StreamStatus::Running,
            final_value: None,
            error: None,
        };
        let shared = Arc::new(RwLock::new(state));
        let notify = Arc::new(Notify::new());
        let readers = Arc::new(AtomicUsize::new(0));
        let writers = Arc::new(AtomicUsize::new(1));
        let stream = SegmentStream {
            state: shared.clone(),
            notify: notify.clone(),
            readers: readers.clone(),
        };
        let writer = StreamWriter {
            state: shared,
            notify,
            readers,
            writers,
        };
        (stream, writer)
    }

    /// Create a new [`StreamReader`] for this stream.
    pub fn reader(&self) -> StreamReader {
        self.readers.fetch_add(1, Ordering::Relaxed);
        StreamReader {
            stream: self.clone(),
            cursor: 0,
        }
    }

    #[doc(hidden)]
    pub fn debug_state_weak(&self) -> StreamStateWeak {
        StreamStateWeak {
            inner: Arc::downgrade(&self.state),
        }
    }

    #[doc(hidden)]
    pub fn debug_readers_count(&self) -> usize {
        self.readers.load(Ordering::Relaxed)
    }

    /// Block until the stream completes and return the final aggregated segment.
    pub async fn collect(&self) -> Result<Segment, String> {
        loop {
            let snapshot = self.state.read().await;
            match snapshot.status {
                StreamStatus::Completed => {
                    return Ok(snapshot.final_value.clone().unwrap_or(Segment::None));
                }
                StreamStatus::Failed => {
                    return Err(snapshot
                        .error
                        .clone()
                        .unwrap_or_else(|| "stream failed".into()));
                }
                StreamStatus::Running => {}
            }
            drop(snapshot);
            self.notify.notified().await;
        }
    }

    /// Take a non-blocking snapshot of the current stream value.
    pub fn snapshot_segment(&self) -> Segment {
        match self.state.try_read() {
            Ok(snapshot) => match snapshot.status {
                StreamStatus::Completed => snapshot.final_value.clone().unwrap_or(Segment::None),
                StreamStatus::Failed => Segment::None,
                StreamStatus::Running => Segment::Array(snapshot.chunks.clone()),
            },
            Err(_) => Segment::None,
        }
    }

    /// Return the current stream status (non-blocking).
    pub fn status(&self) -> StreamStatus {
        self.state
            .try_read()
            .map(|s| s.status.clone())
            .unwrap_or(StreamStatus::Running)
    }

    /// Return the current stream status (async, acquires read lock).
    pub async fn status_async(&self) -> StreamStatus {
        self.state.read().await.status.clone()
    }

    /// Return a snapshot of all chunks received so far (non-blocking).
    pub fn chunks(&self) -> Vec<Segment> {
        self.state
            .try_read()
            .map(|s| s.chunks.items.clone())
            .unwrap_or_default()
    }

    /// Return a snapshot of all chunks received so far (async).
    pub async fn chunks_async(&self) -> Vec<Segment> {
        self.state.read().await.chunks.items.clone()
    }

    /// Take an async snapshot of the current stream value.
    pub async fn snapshot_segment_async(&self) -> Segment {
        let snapshot = self.state.read().await;
        match snapshot.status {
            StreamStatus::Completed => snapshot.final_value.clone().unwrap_or(Segment::None),
            StreamStatus::Failed => Segment::None,
            StreamStatus::Running => Segment::Array(snapshot.chunks.clone()),
        }
    }

    pub(crate) fn snapshot_status(
        &self,
    ) -> (StreamStatus, Vec<Segment>, Option<Segment>, Option<String>) {
        match self.state.try_read() {
            Ok(snapshot) => (
                snapshot.status.clone(),
                snapshot.chunks.items.clone(),
                snapshot.final_value.clone(),
                snapshot.error.clone(),
            ),
            Err(_) => (StreamStatus::Running, Vec::new(), None, None),
        }
    }

    fn is_empty(&self) -> bool {
        match self.state.try_read() {
            Ok(snapshot) => snapshot.chunks.is_empty() && snapshot.final_value.is_none(),
            Err(_) => false,
        }
    }
}

impl StreamWriter {
    /// Send a chunk to the stream. No-op if the stream has already ended.
    pub async fn send(&self, chunk: Segment) {
        let mut state = self.state.write().await;
        if state.status != StreamStatus::Running {
            return;
        }
        let chunk_bytes = chunk.estimate_bytes();
        if let Some(max_chunks) = state.limits.max_chunks {
            if state.chunks.len() >= max_chunks {
                state.status = StreamStatus::Failed;
                state.error = Some("stream buffer exceeded max chunks".to_string());
                drop(state);
                self.notify_readers();
                return;
            }
        }
        if let Some(max_bytes) = state.limits.max_buffer_bytes {
            if state.buffer_bytes + chunk_bytes > max_bytes {
                state.status = StreamStatus::Failed;
                state.error = Some("stream buffer exceeded max bytes".to_string());
                drop(state);
                self.notify_readers();
                return;
            }
        }

        let chunks = Arc::make_mut(&mut state.chunks);
        chunks.push(chunk);
        state.buffer_bytes += chunk_bytes;
        drop(state);
        self.notify_readers();
    }

    /// Finalize the stream with an aggregated value.
    pub async fn end(&self, final_value: Segment) {
        let mut state = self.state.write().await;
        if state.status != StreamStatus::Running {
            return;
        }
        state.status = StreamStatus::Completed;
        state.final_value = Some(final_value);
        drop(state);
        self.notify_readers();
    }

    /// Finalize the stream without a final value.
    pub async fn finish(&self) {
        self.end(Segment::None).await;
    }

    /// Mark the stream as failed with an error message.
    pub async fn error(&self, message: String) {
        let mut state = self.state.write().await;
        if state.status != StreamStatus::Running {
            return;
        }
        state.status = StreamStatus::Failed;
        state.error = Some(message);
        drop(state);
        self.notify_readers();
    }

    fn notify_readers(&self) {
        if self.readers.load(Ordering::Relaxed) <= 1 {
            self.notify.notify_one();
        } else {
            self.notify.notify_waiters();
        }
    }
}

impl Clone for StreamWriter {
    fn clone(&self) -> Self {
        self.writers.fetch_add(1, Ordering::Relaxed);
        StreamWriter {
            state: self.state.clone(),
            notify: self.notify.clone(),
            readers: self.readers.clone(),
            writers: self.writers.clone(),
        }
    }
}

impl Drop for StreamWriter {
    fn drop(&mut self) {
        if self.writers.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        if let Ok(mut state) = self.state.try_write() {
            if state.status == StreamStatus::Running {
                state.status = StreamStatus::Failed;
                state.error = Some("stream writer dropped without calling end()".to_string());
            }
        }
        self.notify.notify_waiters();
    }
}

impl Drop for StreamReader {
    fn drop(&mut self) {
        self.stream.readers.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Clone for StreamReader {
    fn clone(&self) -> Self {
        self.stream.readers.fetch_add(1, Ordering::Relaxed);
        StreamReader {
            stream: self.stream.clone(),
            cursor: self.cursor,
        }
    }
}

impl StreamReader {
    pub async fn next(&mut self) -> Option<StreamEvent> {
        loop {
            let snapshot = self.stream.state.read().await;
            if self.cursor < snapshot.chunks.len() {
                let item = snapshot.chunks[self.cursor].clone();
                self.cursor += 1;
                return Some(StreamEvent::Chunk(item));
            }
            match snapshot.status {
                StreamStatus::Running => {
                    drop(snapshot);
                    self.stream.notify.notified().await;
                }
                StreamStatus::Completed => {
                    return Some(StreamEvent::End(
                        snapshot.final_value.clone().unwrap_or(Segment::None),
                    ));
                }
                StreamStatus::Failed => {
                    return Some(StreamEvent::Error(
                        snapshot
                            .error
                            .clone()
                            .unwrap_or_else(|| "stream failed".into()),
                    ));
                }
            }
        }
    }
}

impl Segment {
    /// Return the [`SegmentType`] that corresponds to this segment’s runtime value.
    pub fn segment_type(&self) -> SegmentType {
        match self {
            Segment::None => SegmentType::Any,
            Segment::String(_) => SegmentType::String,
            Segment::Integer(_) | Segment::Float(_) => SegmentType::Number,
            Segment::Boolean(_) => SegmentType::Boolean,
            Segment::Object(_) => SegmentType::Object,
            Segment::ArrayString(_) => SegmentType::ArrayString,
            Segment::Array(_) => SegmentType::Array,
            Segment::Stream(_) => SegmentType::Any,
            Segment::File(_) => SegmentType::File,
            Segment::ArrayFile(_) => SegmentType::ArrayFile,
        }
    }

    /// Check whether this segment is compatible with the given DSL type.
    pub fn matches_type(&self, t: &SegmentType) -> bool {
        match t {
            SegmentType::Any => true,
            SegmentType::File => matches!(self, Segment::File(_)),
            SegmentType::ArrayFile => matches!(self, Segment::ArrayFile(_)),
            SegmentType::ArrayNumber | SegmentType::ArrayObject | SegmentType::Array => {
                matches!(self, Segment::Array(_) | Segment::ArrayString(_))
            }
            _ => self.segment_type() == *t,
        }
    }
}

impl Serialize for Segment {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.snapshot_to_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Segment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = Value::deserialize(deserializer)?;
        Ok(Segment::from_value(&v))
    }
}

impl Segment {
    /// Explicitly construct a string array.
    pub fn string_array(items: Vec<String>) -> Self {
        Segment::ArrayString(Arc::new(items))
    }

    /// Convert Segment → serde_json::Value, snapshotting streams if needed.
    pub fn to_value(&self) -> Value {
        match self {
            Segment::None => Value::Null,
            Segment::String(s) => Value::String(s.clone()),
            Segment::Integer(i) => serde_json::json!(*i),
            Segment::Float(f) => serde_json::json!(*f),
            Segment::Boolean(b) => Value::Bool(*b),
            Segment::Object(map) => map.to_value(),
            Segment::ArrayString(v) => {
                Value::Array(v.iter().map(|s| Value::String(s.clone())).collect())
            }
            Segment::Array(v) => v.to_value(),
            Segment::Stream(_) => self.snapshot_to_value(),
            Segment::File(file) => serde_json::to_value(file.as_ref()).unwrap_or(Value::Null),
            Segment::ArrayFile(files) => Value::Array(
                files
                    .iter()
                    .map(|f| serde_json::to_value(f).unwrap_or(Value::Null))
                    .collect(),
            ),
        }
    }

    /// Convert Segment → serde_json::Value, consuming self (streams are snapshotted).
    ///
    /// This can reduce cloning on temporary segments (e.g. stream snapshots) by
    /// moving keys/items when the underlying `Arc` is uniquely owned.
    pub fn into_value(self) -> Value {
        match self {
            Segment::None => Value::Null,
            Segment::String(s) => Value::String(s),
            Segment::Integer(i) => serde_json::json!(i),
            Segment::Float(f) => serde_json::json!(f),
            Segment::Boolean(b) => Value::Bool(b),
            Segment::Object(map) => match Arc::try_unwrap(map) {
                Ok(obj) => obj.into_value(),
                Err(arc) => arc.to_value(),
            },
            Segment::ArrayString(v) => match Arc::try_unwrap(v) {
                Ok(items) => Value::Array(items.into_iter().map(Value::String).collect()),
                Err(arc) => Value::Array(arc.iter().map(|s| Value::String(s.clone())).collect()),
            },
            Segment::Array(v) => match Arc::try_unwrap(v) {
                Ok(arr) => arr.into_value(),
                Err(arc) => arc.to_value(),
            },
            Segment::Stream(stream) => {
                let snapshot = stream.snapshot_segment();
                snapshot.snapshot_to_value()
            }
            Segment::File(file) => serde_json::to_value(file.as_ref()).unwrap_or(Value::Null),
            Segment::ArrayFile(files) => Value::Array(
                files
                    .iter()
                    .map(|f| serde_json::to_value(f).unwrap_or(Value::Null))
                    .collect(),
            ),
        }
    }

    /// Convert Segment → serde_json::Value, snapshotting streams if needed.
    pub fn snapshot_to_value(&self) -> Value {
        match self {
            Segment::Stream(stream) => match stream.snapshot_segment() {
                Segment::Array(arr) => {
                    Value::Array(arr.iter().map(|s| s.snapshot_to_value()).collect())
                }
                other => other.snapshot_to_value(),
            },
            other => other.to_value(),
        }
    }

    /// Create Segment from serde_json::Value
    pub fn from_value(v: &Value) -> Self {
        match v {
            Value::Null => Segment::None,
            Value::Bool(b) => Segment::Boolean(*b),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Segment::Integer(i)
                } else {
                    Segment::Float(n.as_f64().unwrap_or(0.0))
                }
            }
            Value::String(s) => Segment::String(s.clone()),
            Value::Array(arr) => {
                if arr.is_empty() {
                    Segment::Array(Arc::new(SegmentArray::new(Vec::new())))
                } else if arr.iter().all(|v| v.is_string()) {
                    let items = arr
                        .iter()
                        .map(|v| v.as_str().unwrap_or_default().to_string())
                        .collect();
                    Segment::ArrayString(Arc::new(items))
                } else {
                    Segment::Array(Arc::new(SegmentArray::new(
                        arr.iter().map(Segment::from_value).collect(),
                    )))
                }
            }
            Value::Object(map) => {
                let m: HashMap<String, Segment> = map
                    .iter()
                    .map(|(k, v)| (k.clone(), Segment::from_value(v)))
                    .collect();
                Segment::Object(Arc::new(SegmentObject::new(m)))
            }
        }
    }

    /// Return `true` if this segment is `None` (null).
    pub fn is_none(&self) -> bool {
        matches!(self, Segment::None)
    }

    /// Try to extract a `String` representation of this segment.
    ///
    /// Primitives are converted to their string forms; composite types return `None`.
    pub fn as_string(&self) -> Option<String> {
        match self {
            Segment::String(s) => Some(s.clone()),
            Segment::Integer(i) => Some(i.to_string()),
            Segment::Float(f) => Some(f.to_string()),
            Segment::Boolean(b) => Some(b.to_string()),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Segment::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Segment::Integer(i) => Some(*i),
            Segment::Float(f) => Some(*f as i64),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        self.as_i64()
            .and_then(|v| if v >= 0 { Some(v as u64) } else { None })
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Segment::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Segment>> {
        match self {
            Segment::Array(items) => Some(items.deref()),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&HashMap<String, Segment>> {
        match self {
            Segment::Object(map) => Some(map.deref()),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Segment> {
        self.as_object().and_then(|m| m.get(key))
    }

    /// Return a human-readable display string, resolving streams if needed.
    pub fn to_display_string(&self) -> String {
        match self {
            Segment::None => String::new(),
            Segment::String(s) => s.clone(),
            Segment::Integer(i) => i.to_string(),
            Segment::Float(f) => f.to_string(),
            Segment::Boolean(b) => b.to_string(),
            Segment::Stream(stream) => match stream.snapshot_status() {
                (StreamStatus::Completed, _chunks, final_value, _) => {
                    final_value.unwrap_or(Segment::None).to_display_string()
                }
                (StreamStatus::Failed, _chunks, _final_value, error) => {
                    format!("[stream error: {}]", error.unwrap_or_default())
                }
                (StreamStatus::Running, chunks, _final_value, _) => chunks
                    .iter()
                    .map(|c| c.to_display_string())
                    .collect::<Vec<_>>()
                    .join(""),
            },
            other => serde_json::to_string(&other.to_value()).unwrap_or_default(),
        }
    }

    /// Try to extract a `f64` from this segment (integers, floats, or parseable strings).
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Segment::Integer(i) => Some(*i as f64),
            Segment::Float(f) => Some(*f),
            Segment::String(s) => s.parse::<f64>().ok(),
            _ => None,
        }
    }

    /// Return `true` if this segment is empty (null, empty string, empty collection, etc.).
    pub fn is_empty(&self) -> bool {
        match self {
            Segment::None => true,
            Segment::String(s) => s.is_empty(),
            Segment::ArrayString(v) => v.is_empty(),
            Segment::Array(v) => v.is_empty(),
            Segment::Object(map) => map.is_empty(),
            Segment::Stream(stream) => stream.is_empty(),
            Segment::File(_) => false,
            Segment::ArrayFile(v) => v.is_empty(),
            _ => false,
        }
    }

    /// Estimate the memory footprint of this segment in bytes.
    pub fn estimate_bytes(&self) -> usize {
        match self {
            Segment::None => 0,
            Segment::String(s) => s.len(),
            Segment::Integer(_) | Segment::Float(_) | Segment::Boolean(_) => 8,
            Segment::ArrayString(items) => items.iter().map(|s| s.len()).sum(),
            Segment::Array(items) => items.iter().map(|s| s.estimate_bytes()).sum(),
            Segment::Object(map) => map.iter().map(|(k, v)| k.len() + v.estimate_bytes()).sum(),
            Segment::Stream(stream) => stream.snapshot_segment().estimate_bytes(),
            Segment::File(file) => {
                file.name.len()
                    + file.mime_type.len()
                    + file.extension.as_ref().map(|v| v.len()).unwrap_or(0)
                    + file.url.as_ref().map(|v| v.len()).unwrap_or(0)
                    + file.id.as_ref().map(|v| v.len()).unwrap_or(0)
                    + file.hash.as_ref().map(|v| v.len()).unwrap_or(0)
                    + 32
            }
            Segment::ArrayFile(files) => files
                .iter()
                .map(|f| {
                    f.name.len()
                        + f.mime_type.len()
                        + f.extension.as_ref().map(|v| v.len()).unwrap_or(0)
                        + f.url.as_ref().map(|v| v.len()).unwrap_or(0)
                        + f.id.as_ref().map(|v| v.len()).unwrap_or(0)
                        + f.hash.as_ref().map(|v| v.len()).unwrap_or(0)
                        + 32
                })
                .sum(),
        }
    }
}

impl PartialEq for Segment {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Segment::None, Segment::None) => true,
            (Segment::String(a), Segment::String(b)) => a == b,
            (Segment::Integer(a), Segment::Integer(b)) => a == b,
            (Segment::Float(a), Segment::Float(b)) => (a - b).abs() < 1e-10,
            (Segment::Integer(a), Segment::Float(b)) | (Segment::Float(b), Segment::Integer(a)) => {
                (*a as f64 - b).abs() < 1e-10
            }
            (Segment::ArrayString(a), Segment::ArrayString(b)) => a == b,
            _ => self.snapshot_to_value() == other.snapshot_to_value(),
        }
    }
}

impl PartialEq<Value> for Segment {
    fn eq(&self, other: &Value) -> bool {
        self.snapshot_to_value() == *other
    }
}

impl PartialEq<&Value> for Segment {
    fn eq(&self, other: &&Value) -> bool {
        self.snapshot_to_value() == **other
    }
}

impl PartialEq<Segment> for Value {
    fn eq(&self, other: &Segment) -> bool {
        *self == other.snapshot_to_value()
    }
}

impl PartialEq<Segment> for &Value {
    fn eq(&self, other: &Segment) -> bool {
        **self == other.snapshot_to_value()
    }
}

impl PartialEq<i32> for Segment {
    fn eq(&self, other: &i32) -> bool {
        self.as_i64() == Some(*other as i64)
    }
}

impl PartialEq<i64> for Segment {
    fn eq(&self, other: &i64) -> bool {
        self.as_i64() == Some(*other)
    }
}

impl PartialEq<u64> for Segment {
    fn eq(&self, other: &u64) -> bool {
        self.as_u64() == Some(*other)
    }
}

impl PartialEq<usize> for Segment {
    fn eq(&self, other: &usize) -> bool {
        self.as_u64() == Some(*other as u64)
    }
}

impl PartialEq<bool> for Segment {
    fn eq(&self, other: &bool) -> bool {
        self.as_bool() == Some(*other)
    }
}

impl PartialEq<&str> for Segment {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == Some(*other)
    }
}

impl Index<&str> for Segment {
    type Output = Segment;

    fn index(&self, index: &str) -> &Self::Output {
        match self {
            Segment::Object(map) => map
                .get(index)
                .unwrap_or_else(|| panic!("key '{}' not found in Segment::Object", index)),
            _ => panic!("cannot index Segment with key '{}': not an object", index),
        }
    }
}

impl std::fmt::Display for Segment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_display_string())
    }
}
