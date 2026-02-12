//! Variable pool and segment type system.
//!
//! This module implements the Dify-compatible variable storage and type system.
//! Variables are addressed by [`Selector`] (a `(node_id, variable_name)` pair) and
//! stored as [`Segment`] values in a copy-on-write [`VariablePool`].
//!
//! # Streaming
//!
//! [`SegmentStream`] supports LLM streaming responses. A stream is created via
//! [`SegmentStream::channel`], which returns a `(SegmentStream, StreamWriter)` pair.
//! Downstream nodes can read chunks incrementally through [`StreamReader`].

use compact_str::CompactString;
use im::HashMap as ImHashMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use tokio::sync::{Notify, RwLock};

/// Sentinel node ID used for scoped (local) variables within sub-graphs.
pub const SCOPE_NODE_ID: &str = "__scope__";

/// A two-part variable address: `(node_id, variable_name)`.
///
/// Selectors are used throughout the engine to reference variables in the
/// [`VariablePool`]. They can be parsed from JSON arrays (`["node", "var"]`)
/// or dot-separated strings (`"node.var"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Selector {
    node_id: String,
    variable_name: String,
}

impl Selector {
    /// Create a new selector from a node ID and variable name.
    pub fn new(node_id: impl Into<String>, variable_name: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            variable_name: variable_name.into(),
        }
    }

    /// Parse a selector from a JSON value (string or array of strings).
    pub fn parse_value(value: &Value) -> Option<Self> {
        match value {
            Value::Array(arr) => {
                let mut parts = Vec::with_capacity(arr.len());
                for v in arr {
                    if let Some(s) = v.as_str() {
                        if !s.is_empty() {
                            parts.push(s.to_string());
                        }
                    } else {
                        return None;
                    }
                }
                Self::from_parts(parts)
            }
            Value::String(s) => Self::parse_str(s),
            _ => None,
        }
    }

    /// Parse a selector from a dot-separated string (e.g. `"node_id.var_name"`).
    pub fn parse_str(selector: &str) -> Option<Self> {
        let parts: Vec<String> = selector
            .split('.')
            .filter(|p| !p.is_empty())
            .map(|p| p.to_string())
            .collect();
        Self::from_parts(parts)
    }

    fn from_parts(parts: Vec<String>) -> Option<Self> {
        match parts.len() {
            1 => Some(Self::new(SCOPE_NODE_ID, parts[0].clone())),
            2 => Some(Self::new(parts[0].clone(), parts[1].clone())),
            _ => None,
        }
    }

    /// Returns the node ID component of this selector.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Returns the variable name component of this selector.
    pub fn variable_name(&self) -> &str {
        &self.variable_name
    }

    /// Returns `true` if either the node ID or variable name is empty.
    pub fn is_empty(&self) -> bool {
        self.node_id.is_empty() || self.variable_name.is_empty()
    }

    pub(crate) fn pool_key(&self) -> CompactString {
        VariablePool::make_key(&self.node_id, &self.variable_name)
    }
}

impl Serialize for Selector {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let parts = vec![self.node_id.clone(), self.variable_name.clone()];
        parts.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Selector {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SelectorVisitor;

        impl<'de> serde::de::Visitor<'de> for SelectorVisitor {
            type Value = Selector;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("selector string like 'node.var' or string array")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Selector::parse_str(v).ok_or_else(|| E::custom("invalid selector string"))
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut parts = Vec::new();
                while let Some(value) = seq.next_element::<String>()? {
                    if !value.is_empty() {
                        parts.push(value);
                    }
                }
                Selector::from_parts(parts)
                    .ok_or_else(|| serde::de::Error::custom("invalid selector array"))
            }
        }

        deserializer.deserialize_any(SelectorVisitor)
    }
}

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

    fn to_value(&self) -> Value {
        self.cached_value
            .get_or_init(|| Value::Array(self.items.iter().map(|s| s.to_value()).collect()))
            .clone()
    }

    fn into_value(self) -> Value {
        if let Some(v) = self.cached_value.into_inner() {
            return v;
        }
        Value::Array(self.items.into_iter().map(|s| s.into_value()).collect())
    }

    fn push(&mut self, value: Segment) {
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

    fn to_value(&self) -> Value {
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

    fn into_value(self) -> Value {
        if let Some(v) = self.cached_value.into_inner() {
            return v;
        }
        Value::Object(
            self
                .entries
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
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileSegment {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub transfer_method: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub extension: Option<String>,
    #[serde(default)]
    pub size: Option<i64>,
}

impl FileSegment {
    /// Convert this file metadata into a [`Segment::Object`].
    pub fn to_segment(&self) -> Segment {
        let value = serde_json::to_value(self).unwrap_or(Value::Null);
        Segment::from_value(&value)
    }

    /// Try to reconstruct a `FileSegment` from a [`Segment`] snapshot.
    pub fn from_segment(seg: &Segment) -> Option<Self> {
        serde_json::from_value(seg.snapshot_to_value()).ok()
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
                    return Err(snapshot.error.clone().unwrap_or_else(|| "stream failed".into()));
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

    fn snapshot_status(&self) -> (StreamStatus, Vec<Segment>, Option<Segment>, Option<String>) {
        match self.state.try_read() {
            Ok(snapshot) => (
                snapshot.status.clone(),
                snapshot.chunks.items.clone(),
                snapshot.final_value.clone(),
                snapshot.error.clone(),
            ),
            Err(_) => (
                StreamStatus::Running,
                Vec::new(),
                None,
                None,
            ),
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

// ================================
// SegmentType – DSL-facing type markers
// ================================

/// DSL-level type marker for workflow variables.
///
/// These correspond to the type strings used in the Dify DSL (`"string"`,
/// `"number"`, `"array[string]"`, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentType {
    String,
    Number,
    Boolean,
    Object,
    ArrayString,
    ArrayNumber,
    ArrayObject,
    File,
    ArrayFile,
    Array,
    Any,
}

impl SegmentType {
    /// Parse a DSL type string (e.g. `"string"`, `"array[object]"`) into a `SegmentType`.
    pub fn from_dsl_type(t: &str) -> Option<Self> {
        match t.trim().to_lowercase().as_str() {
            "string" => Some(SegmentType::String),
            "number" => Some(SegmentType::Number),
            "boolean" => Some(SegmentType::Boolean),
            "object" => Some(SegmentType::Object),
            "array[string]" => Some(SegmentType::ArrayString),
            "array[number]" => Some(SegmentType::ArrayNumber),
            "array[object]" => Some(SegmentType::ArrayObject),
            "file" => Some(SegmentType::File),
            "array[file]" => Some(SegmentType::ArrayFile),
            _ => None,
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
        }
    }

    /// Check whether this segment is compatible with the given DSL type.
    pub fn matches_type(&self, t: &SegmentType) -> bool {
        match t {
            SegmentType::Any => true,
            SegmentType::File => matches!(self, Segment::Object(_)),
            SegmentType::ArrayNumber
            | SegmentType::ArrayObject
            | SegmentType::ArrayFile
            | SegmentType::Array => matches!(self, Segment::Array(_) | Segment::ArrayString(_)),
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
            Segment::ArrayString(v) => Value::Array(v.iter().map(|s| Value::String(s.clone())).collect()),
            Segment::Array(v) => v.to_value(),
            Segment::Stream(_) => self.snapshot_to_value(),
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
        }
    }

    /// Convert Segment → serde_json::Value, snapshotting streams if needed.
    pub fn snapshot_to_value(&self) -> Value {
        match self {
            Segment::Stream(stream) => match stream.snapshot_segment() {
                Segment::Array(arr) => Value::Array(arr.iter().map(|s| s.snapshot_to_value()).collect()),
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

    /// Return a human-readable display string, resolving streams if needed.
    pub fn to_display_string(&self) -> String {
        match self {
            Segment::None => String::new(),
            Segment::String(s) => s.clone(),
            Segment::Integer(i) => i.to_string(),
            Segment::Float(f) => f.to_string(),
            Segment::Boolean(b) => b.to_string(),
            Segment::Stream(stream) => match stream.snapshot_status() {
                (StreamStatus::Completed, _chunks, final_value, _) => final_value
                    .unwrap_or(Segment::None)
                    .to_display_string(),
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
            Segment::Object(map) => map
                .iter()
                .map(|(k, v)| k.len() + v.estimate_bytes())
                .sum(),
            Segment::Stream(stream) => stream.snapshot_segment().estimate_bytes(),
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

impl std::fmt::Display for Segment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_display_string())
    }
}

// ================================
// VariablePool – Dify-compatible
// Key: "node_id:variable_name"
// ================================

/// Copy-on-write variable storage for a workflow execution.
///
/// Variables are keyed by `"node_id:variable_name"` and stored as [`Segment`]
/// values. The pool uses [`im::HashMap`] for efficient structural sharing when
/// forking pools (e.g. for sub-graph / iteration execution).
#[derive(Debug, Clone)]
pub struct VariablePool {
    variables: ImHashMap<CompactString, Segment>,
    #[cfg(feature = "security")]
    selector_validation: Option<crate::security::validation::SelectorValidation>,
    #[cfg(not(feature = "security"))]
    selector_validation: Option<()>,
}

impl VariablePool {
    /// Create an empty variable pool.
    pub fn new() -> Self {
        VariablePool {
            variables: ImHashMap::new(),
            #[cfg(feature = "security")]
            selector_validation: None,
            #[cfg(not(feature = "security"))]
            selector_validation: None,
        }
    }

    #[cfg(feature = "security")]
    pub fn new_with_selector_validation(
        selector_validation: Option<crate::security::validation::SelectorValidation>,
    ) -> Self {
        VariablePool {
            variables: ImHashMap::new(),
            selector_validation,
        }
    }

    #[cfg(not(feature = "security"))]
    pub fn new_with_selector_validation(_selector_validation: Option<()>) -> Self {
        VariablePool {
            variables: ImHashMap::new(),
            selector_validation: None,
        }
    }

    #[cfg(feature = "security")]
    pub fn set_selector_validation(
        &mut self,
        selector_validation: Option<crate::security::validation::SelectorValidation>,
    ) {
        self.selector_validation = selector_validation;
    }

    /// Return the number of variables in the pool.
    pub fn len(&self) -> usize {
        self.variables.len()
    }

    /// Estimate the total memory footprint of all variables in bytes.
    pub fn estimate_total_bytes(&self) -> usize {
        self.variables
            .iter()
            .map(|(k, v)| k.len() + v.estimate_bytes())
            .sum()
    }

    /// Build key from node_id and variable name.
    pub fn make_key(node_id: &str, var_name: &str) -> CompactString {
        let mut key = CompactString::with_capacity(node_id.len() + 1 + var_name.len());
        key.push_str(node_id);
        key.push(':');
        key.push_str(var_name);
        key
    }

    fn key_prefix(node_id: &str) -> CompactString {
        let mut prefix = CompactString::with_capacity(node_id.len() + 1);
        prefix.push_str(node_id);
        prefix.push(':');
        prefix
    }

    #[cfg(feature = "security")]
    fn selector_allowed(&self, selector: &Selector) -> bool {
        let Some(cfg) = &self.selector_validation else {
            return true;
        };
        if 2 > cfg.max_depth {
            return false;
        }

        let total_len = selector.node_id().len() + 1 + selector.variable_name().len();
        if total_len > cfg.max_length {
            return false;
        }

        if cfg.allowed_prefixes.contains("*") {
            return true;
        }
        let prefix = selector.node_id();
        cfg.allowed_prefixes.contains(prefix)
    }

    /// Get variable by selector: (node_id, var_name)
    pub fn get(&self, selector: &Selector) -> Segment {
        #[cfg(feature = "security")]
        if !self.selector_allowed(selector) {
            return Segment::None;
        }
        self.variables
            .get(&selector.pool_key())
            .cloned()
            .unwrap_or(Segment::None)
    }

    /// Get variable and resolve Stream by collecting it.
    pub async fn get_resolved(&self, selector: &Selector) -> Segment {
        match self.get(selector) {
            Segment::Stream(stream) => stream.collect().await.unwrap_or(Segment::None),
            other => other,
        }
    }

    /// Get variable and resolve Stream, returning serde_json::Value.
    pub async fn get_resolved_value(&self, selector: &Selector) -> Value {
        self.get_resolved(selector).await.to_value()
    }

    /// Set a single variable
    pub fn set(&mut self, selector: &Selector, value: Segment) {
        self.variables.insert(selector.pool_key(), value);
    }

    #[cfg(feature = "security")]
    pub fn set_checked(
        &mut self,
        selector: &Selector,
        value: Segment,
        max_entries: usize,
        max_memory_bytes: usize,
    ) -> Result<(), crate::security::QuotaError> {
        if self.variables.len() >= max_entries {
            return Err(crate::security::QuotaError::VariablePoolTooLarge {
                max_entries,
                current: self.variables.len(),
            });
        }
        let estimated_size = self.estimate_total_bytes() + value.estimate_bytes();
        if estimated_size > max_memory_bytes {
            return Err(crate::security::QuotaError::VariablePoolMemoryExceeded {
                max_bytes: max_memory_bytes,
                current: estimated_size,
            });
        }
        self.set(selector, value);
        Ok(())
    }

    /// Set node outputs as (node_id, key) -> value
    pub fn set_node_outputs(&mut self, node_id: &str, outputs: &HashMap<String, Value>) {
        for (key, val) in outputs {
            let seg = Segment::from_value(val);
            self.variables.insert(Self::make_key(node_id, key), seg);
        }
    }

    /// Set node outputs from Segment map
    pub fn set_node_segment_outputs(&mut self, node_id: &str, outputs: &HashMap<String, Segment>) {
        for (key, val) in outputs {
            self.variables
                .insert(Self::make_key(node_id, key), val.clone());
        }
    }

    /// Check if variable exists and is not None
    pub fn has(&self, selector: &Selector) -> bool {
        #[cfg(feature = "security")]
        if !self.selector_allowed(selector) {
            return false;
        }
        self.variables
            .get(&selector.pool_key())
            .map_or(false, |s| !s.is_none())
    }

    /// Get all variables for a given node_id
    pub fn get_node_variables(&self, node_id: &str) -> HashMap<String, Segment> {
        let prefix = Self::key_prefix(node_id);
        self.variables
            .iter()
            .filter(|(key, _)| key.as_str().starts_with(prefix.as_str()))
            .filter_map(|(key, val)| {
                key.split_once(':')
                    .map(|(_, var)| (var.to_string(), val.clone()))
            })
            .collect()
    }

    /// Remove all variables for a given node
    pub fn remove_node(&mut self, node_id: &str) {
        let prefix = Self::key_prefix(node_id);
        self.variables.retain(|key, _| !key.as_str().starts_with(prefix.as_str()));
    }

    /// Append value to an existing array variable
    pub fn append(&mut self, selector: &Selector, value: Segment) {
        let key = selector.pool_key();
        let existing = self
            .variables
            .entry(key)
            .or_insert_with(|| Segment::Array(Arc::new(SegmentArray::new(Vec::new()))));
        match existing {
            Segment::Array(arr) => {
                let arr = Arc::make_mut(arr);
                arr.push(value);
            }
            Segment::ArrayString(arr) => match value {
                Segment::String(s) => {
                    let arr = Arc::make_mut(arr);
                    arr.push(s);
                }
                other => {
                    let mut promoted: Vec<Segment> = arr.iter().cloned().map(Segment::String).collect();
                    promoted.push(other);
                    *existing = Segment::Array(Arc::new(SegmentArray::new(promoted)));
                }
            },
            Segment::String(s) => {
                *s += &value.to_display_string();
            }
            Segment::None => {
                *existing = Segment::Array(Arc::new(SegmentArray::new(vec![value])));
            }
            _ => {
                let old = std::mem::replace(existing, Segment::None);
                *existing = Segment::Array(Arc::new(SegmentArray::new(vec![old, value])));
            }
        }
    }

    /// Clear a variable (set to None)
    pub fn clear(&mut self, selector: &Selector) {
        self.variables.insert(selector.pool_key(), Segment::None);
    }

    /// Snapshot entire pool for serialization
    pub fn snapshot(&self) -> HashMap<String, Segment> {
        self.variables
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }
}

impl Default for VariablePool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_variable_pool_basic() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("node1", "output");
        pool.set(&sel, Segment::String("hello".to_string()));
        let val = pool.get(&sel);
        assert!(matches!(val, Segment::String(s) if s == "hello"));
    }

    #[test]
    fn test_variable_pool_sys() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("sys", "query");
        pool.set(&sel, Segment::String("test query".to_string()));
        assert!(pool.has(&sel));
        let val = pool.get(&sel);
        assert_eq!(val.to_display_string(), "test query");
    }

    #[test]
    fn test_variable_pool_missing() {
        let pool = VariablePool::new();
        let sel = Selector::new("nonexistent", "var");
        assert!(pool.get(&sel).is_none());
        assert!(!pool.has(&sel));
    }

    #[test]
    fn test_set_node_outputs() {
        let mut pool = VariablePool::new();
        let mut outputs = HashMap::new();
        outputs.insert("text".to_string(), Value::String("result".to_string()));
        outputs.insert("count".to_string(), serde_json::json!(42));
        pool.set_node_outputs("node_llm", &outputs);

        let text = pool.get(&Selector::new("node_llm", "text"));
        assert!(matches!(text, Segment::String(s) if s == "result"));

        let count = pool.get(&Selector::new("node_llm", "count"));
        assert!(matches!(count, Segment::Integer(42)));
    }

    #[test]
    fn test_segment_conversion() {
        let seg = Segment::Integer(42);
        let val = seg.to_value();
        assert_eq!(val, serde_json::json!(42));

        let back = Segment::from_value(&val);
        assert!(matches!(back, Segment::Integer(42)));
    }

    #[test]
    fn test_append() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("n", "arr");
        pool.set(&sel, Segment::Array(Arc::new(SegmentArray::new(Vec::new()))));
        pool.append(&sel, Segment::Integer(1));
        pool.append(&sel, Segment::Integer(2));
        match pool.get(&sel) {
            Segment::Array(v) => assert_eq!(v.len(), 2),
            _ => panic!("Expected Array"),
        }
    }

    #[test]
    fn test_from_value_array_inference() {
        let seg = Segment::from_value(&serde_json::json!(["a", "b"]));
        assert!(matches!(seg, Segment::ArrayString(_)));

        let seg = Segment::from_value(&serde_json::json!([1, 2, 3]));
        assert!(matches!(seg, Segment::Array(_)));

        let seg = Segment::from_value(&serde_json::json!([1, "a"]));
        assert!(matches!(seg, Segment::Array(_)));

        let seg = Segment::from_value(&serde_json::json!([]));
        assert!(matches!(seg, Segment::Array(_)));
    }

    #[test]
    fn test_file_segment_roundtrip() {
        let mut file = FileSegment::default();
        file.transfer_method = "local".into();
        file.url = Some("/tmp/a.txt".into());
        let seg = file.to_segment();
        assert!(matches!(seg, Segment::Object(_)));

        let back = FileSegment::from_segment(&seg).unwrap();
        assert_eq!(back.url, file.url);
        assert_eq!(back.transfer_method, file.transfer_method);
    }

    #[test]
    fn test_append_array_string_promote() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("n", "arr");
        pool.set(&sel, Segment::ArrayString(Arc::new(vec!["a".into()])));
        pool.append(&sel, Segment::Integer(1));
        match pool.get(&sel) {
            Segment::Array(v) => assert_eq!(v.len(), 2),
            _ => panic!("Expected Array after promotion"),
        }
    }

    #[test]
    fn test_segment_type_mapping() {
        assert_eq!(SegmentType::from_dsl_type("string"), Some(SegmentType::String));
        assert_eq!(SegmentType::from_dsl_type("number"), Some(SegmentType::Number));
        assert_eq!(SegmentType::from_dsl_type("file"), Some(SegmentType::File));
        assert_eq!(SegmentType::from_dsl_type("invalid"), None);

        assert!(Segment::Integer(42).matches_type(&SegmentType::Number));
        assert!(Segment::Float(3.14).matches_type(&SegmentType::Number));
        assert!(!Segment::String("42".into()).matches_type(&SegmentType::Number));
    }

    #[tokio::test]
    async fn test_stream_basic() {
        let (stream, writer) = SegmentStream::channel();
        writer.send(Segment::String("hello ".into())).await;
        writer.send(Segment::String("world".into())).await;
        writer.end(Segment::String("hello world".into())).await;

        let result = stream.collect().await.unwrap();
        assert_eq!(result.to_display_string(), "hello world");
    }

    #[tokio::test]
    async fn test_stream_multiple_readers() {
        let (stream, writer) = SegmentStream::channel();
        let mut reader1 = stream.reader();
        let mut reader2 = stream.reader();

        writer.send(Segment::String("a".into())).await;
        let e1 = reader1.next().await;
        let e2 = reader2.next().await;
        assert!(matches!(e1, Some(StreamEvent::Chunk(_))));
        assert!(matches!(e2, Some(StreamEvent::Chunk(_))));
    }

    #[tokio::test]
    async fn test_stream_error() {
        let (stream, writer) = SegmentStream::channel();
        writer.error("timeout".into()).await;
        let err = stream.collect().await.unwrap_err();
        assert!(err.contains("timeout"));
    }

    #[tokio::test]
    async fn test_get_resolved_stream() {
        let (stream, writer) = SegmentStream::channel();
        let mut pool = VariablePool::new();
        let sel = Selector::new("n1", "text");
        pool.set(&sel, Segment::Stream(stream));

        tokio::spawn(async move {
            writer.send(Segment::String("hello".into())).await;
            writer.end(Segment::String("hello".into())).await;
        });

        let resolved = pool.get_resolved(&sel).await;
        assert_eq!(resolved.to_display_string(), "hello");
    }

    // ---- Selector tests ----

    #[test]
    fn test_selector_parse_str() {
        let sel = Selector::parse_str("node1.output").unwrap();
        assert_eq!(sel.node_id(), "node1");
        assert_eq!(sel.variable_name(), "output");
    }

    #[test]
    fn test_selector_parse_str_single() {
        let sel = Selector::parse_str("query").unwrap();
        assert_eq!(sel.node_id(), SCOPE_NODE_ID);
        assert_eq!(sel.variable_name(), "query");
    }

    #[test]
    fn test_selector_parse_str_too_deep() {
        assert!(Selector::parse_str("a.b.c").is_none());
    }

    #[test]
    fn test_selector_parse_str_empty() {
        assert!(Selector::parse_str("").is_none());
    }

    #[test]
    fn test_selector_parse_value_array() {
        let val = serde_json::json!(["node1", "output"]);
        let sel = Selector::parse_value(&val).unwrap();
        assert_eq!(sel.node_id(), "node1");
        assert_eq!(sel.variable_name(), "output");
    }

    #[test]
    fn test_selector_parse_value_string() {
        let val = serde_json::json!("node1.output");
        let sel = Selector::parse_value(&val).unwrap();
        assert_eq!(sel.node_id(), "node1");
    }

    #[test]
    fn test_selector_parse_value_invalid() {
        assert!(Selector::parse_value(&serde_json::json!(42)).is_none());
    }

    #[test]
    fn test_selector_is_empty() {
        let sel = Selector::new("", "var");
        assert!(sel.is_empty());
        let sel = Selector::new("node", "");
        assert!(sel.is_empty());
        let sel = Selector::new("node", "var");
        assert!(!sel.is_empty());
    }

    #[test]
    fn test_selector_serde_roundtrip() {
        let sel = Selector::new("n1", "out");
        let json = serde_json::to_string(&sel).unwrap();
        let back: Selector = serde_json::from_str(&json).unwrap();
        assert_eq!(sel, back);
    }

    #[test]
    fn test_selector_deserialize_from_string() {
        let back: Selector = serde_json::from_str(r#""n1.out""#).unwrap();
        assert_eq!(back.node_id(), "n1");
        assert_eq!(back.variable_name(), "out");
    }

    // ---- Segment tests ----

    #[test]
    fn test_segment_as_string() {
        assert_eq!(Segment::String("hi".into()).as_string(), Some("hi".into()));
        assert_eq!(Segment::Integer(42).as_string(), Some("42".into()));
        assert_eq!(Segment::Float(3.14).as_string(), Some("3.14".into()));
        assert_eq!(Segment::Boolean(true).as_string(), Some("true".into()));
        assert_eq!(Segment::None.as_string(), None);
    }

    #[test]
    fn test_segment_as_f64() {
        assert_eq!(Segment::Integer(42).as_f64(), Some(42.0));
        assert_eq!(Segment::Float(3.14).as_f64(), Some(3.14));
        assert_eq!(Segment::String("2.5".into()).as_f64(), Some(2.5));
        assert_eq!(Segment::String("not_num".into()).as_f64(), None);
        assert_eq!(Segment::None.as_f64(), None);
    }

    #[test]
    fn test_segment_is_empty() {
        assert!(Segment::None.is_empty());
        assert!(Segment::String("".into()).is_empty());
        assert!(!Segment::String("a".into()).is_empty());
        assert!(Segment::ArrayString(Arc::new(vec![])).is_empty());
        assert!(!Segment::ArrayString(Arc::new(vec!["a".into()])).is_empty());
        assert!(!Segment::Integer(0).is_empty());
    }

    #[test]
    fn test_segment_estimate_bytes() {
        assert_eq!(Segment::None.estimate_bytes(), 0);
        assert_eq!(Segment::String("hello".into()).estimate_bytes(), 5);
        assert_eq!(Segment::Integer(42).estimate_bytes(), 8);
        assert_eq!(Segment::Boolean(true).estimate_bytes(), 8);
        assert_eq!(Segment::ArrayString(Arc::new(vec!["ab".into(), "cd".into()])).estimate_bytes(), 4);
    }

    #[test]
    fn test_segment_display() {
        assert_eq!(format!("{}", Segment::String("hi".into())), "hi");
        assert_eq!(format!("{}", Segment::Integer(42)), "42");
        assert_eq!(format!("{}", Segment::None), "");
    }

    #[test]
    fn test_segment_partial_eq() {
        assert_eq!(Segment::None, Segment::None);
        assert_eq!(Segment::String("a".into()), Segment::String("a".into()));
        assert_ne!(Segment::String("a".into()), Segment::String("b".into()));
        assert_eq!(Segment::Integer(1), Segment::Integer(1));
        assert_eq!(Segment::Float(1.0), Segment::Float(1.0));
    }

    #[test]
    fn test_segment_string_array() {
        let seg = Segment::string_array(vec!["a".into(), "b".into()]);
        match seg {
            Segment::ArrayString(arr) => assert_eq!(arr.len(), 2),
            _ => panic!("expected ArrayString"),
        }
    }

    #[test]
    fn test_segment_into_value() {
        assert_eq!(Segment::None.into_value(), Value::Null);
        assert_eq!(Segment::String("a".into()).into_value(), Value::String("a".into()));
        assert_eq!(Segment::Integer(1).into_value(), serde_json::json!(1));
        assert_eq!(Segment::Float(2.5).into_value(), serde_json::json!(2.5));
        assert_eq!(Segment::Boolean(true).into_value(), Value::Bool(true));
        let arr = Segment::ArrayString(Arc::new(vec!["x".into()]));
        assert_eq!(arr.into_value(), serde_json::json!(["x"]));
    }

    #[test]
    fn test_segment_from_value_object() {
        let val = serde_json::json!({"key": "val"});
        let seg = Segment::from_value(&val);
        assert!(matches!(seg, Segment::Object(_)));
        assert_eq!(seg.to_value(), val);
    }

    #[test]
    fn test_segment_from_value_float() {
        let val = serde_json::json!(3.14);
        let seg = Segment::from_value(&val);
        assert!(matches!(seg, Segment::Float(_)));
    }

    #[test]
    fn test_segment_type() {
        assert_eq!(Segment::None.segment_type(), SegmentType::Any);
        assert_eq!(Segment::String("a".into()).segment_type(), SegmentType::String);
        assert_eq!(Segment::Integer(1).segment_type(), SegmentType::Number);
        assert_eq!(Segment::Float(1.0).segment_type(), SegmentType::Number);
        assert_eq!(Segment::Boolean(true).segment_type(), SegmentType::Boolean);
    }

    #[test]
    fn test_segment_matches_type() {
        assert!(Segment::Integer(1).matches_type(&SegmentType::Any));
        assert!(Segment::Integer(1).matches_type(&SegmentType::Number));
        assert!(!Segment::Integer(1).matches_type(&SegmentType::String));
        let arr = Segment::Array(Arc::new(SegmentArray::new(vec![])));
        assert!(arr.matches_type(&SegmentType::ArrayNumber));
        assert!(arr.matches_type(&SegmentType::Array));
    }

    #[test]
    fn test_segment_serde() {
        let seg = Segment::Integer(42);
        let json = serde_json::to_value(&seg).unwrap();
        let back: Segment = serde_json::from_value(json).unwrap();
        assert_eq!(back, seg);
    }

    // ---- VariablePool additional tests ----

    #[test]
    fn test_pool_len_and_estimate_bytes() {
        let mut pool = VariablePool::new();
        assert_eq!(pool.len(), 0);
        assert_eq!(pool.estimate_total_bytes(), 0);
        pool.set(&Selector::new("n", "a"), Segment::String("hello".into()));
        assert_eq!(pool.len(), 1);
        assert!(pool.estimate_total_bytes() > 0);
    }

    #[test]
    fn test_pool_set_node_segment_outputs() {
        let mut pool = VariablePool::new();
        let mut outputs = HashMap::new();
        outputs.insert("x".to_string(), Segment::Integer(10));
        outputs.insert("y".to_string(), Segment::String("hi".into()));
        pool.set_node_segment_outputs("node1", &outputs);
        assert_eq!(pool.get(&Selector::new("node1", "x")), Segment::Integer(10));
        assert_eq!(pool.get(&Selector::new("node1", "y")), Segment::String("hi".into()));
    }

    #[test]
    fn test_pool_get_node_variables() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n1", "a"), Segment::Integer(1));
        pool.set(&Selector::new("n1", "b"), Segment::Integer(2));
        pool.set(&Selector::new("n2", "c"), Segment::Integer(3));
        let vars = pool.get_node_variables("n1");
        assert_eq!(vars.len(), 2);
        assert!(vars.contains_key("a"));
        assert!(vars.contains_key("b"));
    }

    #[test]
    fn test_pool_remove_node() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n1", "a"), Segment::Integer(1));
        pool.set(&Selector::new("n1", "b"), Segment::Integer(2));
        pool.set(&Selector::new("n2", "c"), Segment::Integer(3));
        pool.remove_node("n1");
        assert!(pool.get(&Selector::new("n1", "a")).is_none());
        assert!(pool.get(&Selector::new("n1", "b")).is_none());
        assert_eq!(pool.get(&Selector::new("n2", "c")), Segment::Integer(3));
    }

    #[test]
    fn test_pool_clear() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("n", "x");
        pool.set(&sel, Segment::Integer(42));
        pool.clear(&sel);
        assert!(pool.get(&sel).is_none());
    }

    #[test]
    fn test_pool_snapshot() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n", "a"), Segment::Integer(1));
        pool.set(&Selector::new("n", "b"), Segment::String("x".into()));
        let snap = pool.snapshot();
        assert_eq!(snap.len(), 2);
    }

    #[test]
    fn test_pool_append_to_none() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("n", "arr");
        pool.append(&sel, Segment::Integer(1));
        match pool.get(&sel) {
            Segment::Array(v) => assert_eq!(v.len(), 1),
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn test_pool_append_string() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("n", "s");
        pool.set(&sel, Segment::String("hello".into()));
        pool.append(&sel, Segment::String(" world".into()));
        assert_eq!(pool.get(&sel).to_display_string(), "hello world");
    }

    #[test]
    fn test_pool_append_non_array() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("n", "x");
        pool.set(&sel, Segment::Integer(1));
        pool.append(&sel, Segment::Integer(2));
        match pool.get(&sel) {
            Segment::Array(v) => assert_eq!(v.len(), 2),
            _ => panic!("expected Array after appending to non-array"),
        }
    }

    #[test]
    fn test_pool_append_array_string_same_type() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("n", "arr");
        pool.set(&sel, Segment::ArrayString(Arc::new(vec!["a".into()])));
        pool.append(&sel, Segment::String("b".into()));
        match pool.get(&sel) {
            Segment::ArrayString(v) => assert_eq!(v.len(), 2),
            _ => panic!("expected ArrayString"),
        }
    }

    #[tokio::test]
    async fn test_pool_get_resolved_value() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n", "x"), Segment::Integer(42));
        let val = pool.get_resolved_value(&Selector::new("n", "x")).await;
        assert_eq!(val, serde_json::json!(42));
    }

    #[test]
    fn test_pool_default() {
        let pool = VariablePool::default();
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn test_segment_type_from_dsl_type_all() {
        assert_eq!(SegmentType::from_dsl_type("string"), Some(SegmentType::String));
        assert_eq!(SegmentType::from_dsl_type("number"), Some(SegmentType::Number));
        assert_eq!(SegmentType::from_dsl_type("boolean"), Some(SegmentType::Boolean));
        assert_eq!(SegmentType::from_dsl_type("object"), Some(SegmentType::Object));
        assert_eq!(SegmentType::from_dsl_type("array[string]"), Some(SegmentType::ArrayString));
        assert_eq!(SegmentType::from_dsl_type("array[number]"), Some(SegmentType::ArrayNumber));
        assert_eq!(SegmentType::from_dsl_type("array[object]"), Some(SegmentType::ArrayObject));
        assert_eq!(SegmentType::from_dsl_type("file"), Some(SegmentType::File));
        assert_eq!(SegmentType::from_dsl_type("array[file]"), Some(SegmentType::ArrayFile));
        assert_eq!(SegmentType::from_dsl_type("unknown"), None);
    }

    #[tokio::test]
    async fn test_stream_writer_drop_without_end() {
        let (stream, writer) = SegmentStream::channel();
        drop(writer);
        let err = stream.collect().await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_stream_with_limits_max_chunks() {
        let limits = StreamLimits { max_chunks: Some(1), max_buffer_bytes: None };
        let (stream, writer) = SegmentStream::channel_with_limits(limits);
        writer.send(Segment::String("a".into())).await;
        writer.send(Segment::String("b".into())).await;
        let result = stream.collect().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_stream_with_limits_max_bytes() {
        let limits = StreamLimits { max_chunks: None, max_buffer_bytes: Some(5) };
        let (stream, writer) = SegmentStream::channel_with_limits(limits);
        writer.send(Segment::String("hello".into())).await;
        writer.send(Segment::String("world".into())).await;
        let result = stream.collect().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_stream_multiple_writers() {
        let (stream, writer) = SegmentStream::channel();
        let writer2 = writer.clone();
        writer.send(Segment::String("a".into())).await;
        writer2.send(Segment::String("b".into())).await;
        drop(writer);
        writer2.end(Segment::String("ab".into())).await;
        let result = stream.collect().await.unwrap();
        assert_eq!(result.to_display_string(), "ab");
    }

    #[tokio::test]
    async fn test_stream_status_and_chunks() {
        let (stream, writer) = SegmentStream::channel();
        assert_eq!(stream.status(), StreamStatus::Running);
        writer.send(Segment::String("a".into())).await;
        assert_eq!(stream.chunks().len(), 1);
        writer.end(Segment::String("a".into())).await;
        assert_eq!(stream.status(), StreamStatus::Completed);
    }

    #[tokio::test]
    async fn test_stream_snapshot_segment() {
        let (stream, writer) = SegmentStream::channel();
        writer.send(Segment::String("a".into())).await;
        let snap = stream.snapshot_segment();
        assert!(matches!(snap, Segment::Array(_)));
        writer.end(Segment::String("done".into())).await;
        let snap2 = stream.snapshot_segment();
        assert_eq!(snap2.to_display_string(), "done");
    }

    #[test]
    fn test_segment_display_stream_running() {
        let (stream, _writer) = SegmentStream::channel();
        let seg = Segment::Stream(stream);
        // display should not panic on running stream
        let _ = seg.to_display_string();
    }

    #[test]
    fn test_segment_snapshot_to_value_non_stream() {
        let seg = Segment::Integer(42);
        assert_eq!(seg.snapshot_to_value(), serde_json::json!(42));
    }

    #[test]
    fn test_file_segment_default() {
        let fs = FileSegment::default();
        assert!(fs.url.is_none());
        assert!(fs.filename.is_none());
        assert_eq!(fs.transfer_method, "");
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_pool_set_checked() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("n", "a");
        assert!(pool.set_checked(&sel, Segment::Integer(1), 10, 10000).is_ok());
        assert_eq!(pool.len(), 1);
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_pool_set_checked_too_many_entries() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("n", "a");
        pool.set(&sel, Segment::Integer(1));
        let sel2 = Selector::new("n", "b");
        let result = pool.set_checked(&sel2, Segment::Integer(2), 1, 10000);
        assert!(result.is_err());
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_pool_selector_validation() {
        use crate::security::validation::SelectorValidation;
        let cfg = SelectorValidation {
            max_depth: 2,
            max_length: 10,
            allowed_prefixes: std::collections::HashSet::from(["n1".to_string()]),
        };
        let mut pool = VariablePool::new_with_selector_validation(Some(cfg));
        pool.set(&Selector::new("n1", "a"), Segment::Integer(1));
        assert_eq!(pool.get(&Selector::new("n1", "a")), Segment::Integer(1));
        // Disallowed prefix
        assert!(pool.get(&Selector::new("n2", "a")).is_none());
        assert!(!pool.has(&Selector::new("n2", "a")));
    }

    #[test]
    fn test_stream_writer_send_after_end() {
        let (stream, writer) = SegmentStream::channel();
        let w = writer;
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            w.send(Segment::String("chunk1".into())).await;
            w.end(Segment::String("final".into())).await;
            // Send after end should be a no-op (status != Running)
            w.send(Segment::String("nope".into())).await;
        });
        let (status, chunks, _, _) = stream.snapshot_status();
        assert_eq!(chunks.len(), 1);
        assert!(matches!(status, StreamStatus::Completed));
    }

    #[test]
    fn test_stream_writer_double_end() {
        let (stream, writer) = SegmentStream::channel();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            writer.end(Segment::String("a".into())).await;
            writer.end(Segment::String("b".into())).await;
        });
        let (status, _, final_val, _) = stream.snapshot_status();
        assert!(matches!(status, StreamStatus::Completed));
        // second end should be no-op, keep first value
        assert_eq!(final_val, Some(Segment::String("a".into())));
    }

    #[test]
    fn test_stream_writer_error_after_error() {
        let (stream, writer) = SegmentStream::channel();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            writer.error("first error".to_string()).await;
            writer.error("second error".to_string()).await;
        });
        let (status, _, _, err) = stream.snapshot_status();
        assert!(matches!(status, StreamStatus::Failed));
        assert_eq!(err, Some("first error".to_string()));
    }

    #[test]
    fn test_segment_into_value_shared_arc() {
        // When Arc has multiple owners, into_value should still work (clone path)
        let arr = Arc::new(SegmentArray::new(vec![Segment::Integer(1), Segment::Integer(2)]));
        let arr_clone = arr.clone(); // create second owner
        let seg = Segment::Array(arr);
        let val = seg.into_value();
        assert_eq!(val, serde_json::json!([1, 2]));
        // arr_clone is still alive
        assert_eq!(arr_clone.len(), 2);
    }

    #[test]
    fn test_segment_into_value_object_shared_arc() {
        let mut map = std::collections::HashMap::new();
        map.insert("key".to_string(), Segment::String("val".into()));
        let obj = Arc::new(SegmentObject::new(map));
        let obj_clone = obj.clone();
        let seg = Segment::Object(obj);
        let val = seg.into_value();
        assert_eq!(val, serde_json::json!({"key": "val"}));
        assert_eq!(obj_clone.len(), 1);
    }

    #[test]
    fn test_segment_display_stream_failed() {
        let (stream, writer) = SegmentStream::channel();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            writer.error("some error".to_string()).await;
        });
        let seg = Segment::Stream(stream);
        let display = seg.to_display_string();
        assert!(display.contains("stream error"));
        assert!(display.contains("some error"));
    }

    #[test]
    fn test_segment_display_stream_completed_no_final() {
        let (stream, writer) = SegmentStream::channel();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            writer.end(Segment::None).await;
        });
        let seg = Segment::Stream(stream);
        let display = seg.to_display_string();
        // Completed with None final_value should display empty
        assert!(display.is_empty() || display == "");
    }

    #[test]
    fn test_file_segment_from_segment_non_object() {
        let seg = Segment::String("not an object".into());
        let file = FileSegment::from_segment(&seg);
        assert!(file.is_none());
    }

    #[test]
    fn test_segment_array_into_value_cached() {
        let arr = SegmentArray::new(vec![Segment::Integer(1)]);
        // First call computes value
        let v1 = arr.to_value();
        // Second call should use cache
        let v2 = arr.to_value();
        assert_eq!(v1, v2);
        // into_value when cache is present
        let v3 = arr.into_value();
        assert_eq!(v1, v3);
    }

    #[test]
    fn test_segment_object_into_value_cached() {
        let mut map = std::collections::HashMap::new();
        map.insert("a".to_string(), Segment::Integer(1));
        let obj = SegmentObject::new(map);
        let v1 = obj.to_value();
        let v2 = obj.to_value();
        assert_eq!(v1, v2);
        let v3 = obj.into_value();
        assert_eq!(v1, v3);
    }

    #[test]
    fn test_stream_reader_clone_drop() {
        let (stream, writer) = SegmentStream::channel();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            writer.send(Segment::String("a".into())).await;
            writer.end(Segment::None).await;
        });
        // Just test that clone/drop works without panic
        let reader1 = stream.reader();
        let _reader2 = reader1.clone();
        drop(_reader2);
        // reader1 is still alive
        let chunks = stream.chunks();
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn test_segment_snapshot_to_value_stream() {
        let (stream, writer) = SegmentStream::channel();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            writer.send(Segment::String("hello".into())).await;
            writer.send(Segment::String(" world".into())).await;
            writer.end(Segment::String("hello world".into())).await;
        });
        let seg = Segment::Stream(stream);
        let val = seg.snapshot_to_value();
        // Completed stream with final value should return that value
        assert_eq!(val, serde_json::json!("hello world"));
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_pool_set_checked_memory_limit() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("n", "x");
        // set_checked with very small memory limit
        let result = pool.set_checked(&sel, Segment::String("a very long string that uses bytes".into()), 100, 1);
        assert!(result.is_err());
    }

    // ---- Stream memory/lifecycle tests (moved from tests/memory/) ----

    #[tokio::test]
    async fn test_stream_writer_drop_without_end_collect_fails() {
        let (stream, writer) = SegmentStream::channel();
        writer.send(Segment::String("chunk1".into())).await;
        drop(writer);

        let result = stream.collect().await;
        assert!(result.is_err(), "expected stream to fail after writer drop");
    }

    #[tokio::test]
    async fn test_stream_reader_drop_decrements_count() {
        let (stream, writer) = SegmentStream::channel();
        let r1 = stream.reader();
        let r2 = stream.reader();
        let r3 = stream.reader();

        assert_eq!(stream.debug_readers_count(), 3);

        drop(r1);
        assert_eq!(stream.debug_readers_count(), 2);
        drop(r2);
        assert_eq!(stream.debug_readers_count(), 1);
        drop(r3);
        assert_eq!(stream.debug_readers_count(), 0);

        writer.end(Segment::String("done".into())).await;
        let result = stream.collect().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_stream_arc_cleanup_after_completion() {
        let (stream, writer) = SegmentStream::channel();
        let weak = stream.debug_state_weak();

        writer.end(Segment::String("final".into())).await;
        let _ = stream.collect().await;

        drop(writer);
        drop(stream);

        assert!(weak.is_dropped(), "stream state Arc should be released");
    }

    #[tokio::test]
    async fn test_stream_multiple_readers_partial_drop() {
        let (stream, writer) = SegmentStream::channel();
        let mut r1 = stream.reader();
        let r2 = stream.reader();

        writer.send(Segment::String("chunk".into())).await;
        writer.end(Segment::String("done".into())).await;

        let mut events = Vec::new();
        while let Some(e) = r1.next().await {
            events.push(e);
            if matches!(events.last(), Some(StreamEvent::End(_))) {
                break;
            }
        }

        assert_eq!(events.len(), 2);

        drop(r1);
        drop(r2);
        assert_eq!(stream.debug_readers_count(), 0);
    }

    // ---- Pool structural sharing test (moved from tests/memory/) ----

    #[tokio::test]
    async fn test_pool_clone_structural_sharing() {
        let mut pool = VariablePool::new();
        let mut entries = std::collections::HashMap::new();
        entries.insert("k".to_string(), Segment::String("v".into()));
        let obj = Arc::new(SegmentObject::new(entries));

        pool.set(&Selector::new("node", "obj"), Segment::Object(obj.clone()));
        assert_eq!(Arc::strong_count(&obj), 2);

        let mut clone = pool.clone();
        assert_eq!(Arc::strong_count(&obj), 2);

        clone.set(
            &Selector::new("node", "obj"),
            Segment::String("override".into()),
        );

        assert_eq!(Arc::strong_count(&obj), 2);
    }
}
