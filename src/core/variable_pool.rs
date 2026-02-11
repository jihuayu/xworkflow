use compact_str::CompactString;
use im::HashMap as ImHashMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::sync::{Notify, RwLock};

pub const SCOPE_NODE_ID: &str = "__scope__";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Selector {
    node_id: String,
    variable_name: String,
}

impl Selector {
    pub fn new(node_id: impl Into<String>, variable_name: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            variable_name: variable_name.into(),
        }
    }

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

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn variable_name(&self) -> &str {
        &self.variable_name
    }

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

#[derive(Debug, Default)]
pub struct SegmentArray {
    items: Vec<Segment>,
    cached_value: OnceLock<Value>,
}

impl SegmentArray {
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

#[derive(Debug, Default)]
pub struct SegmentObject {
    entries: HashMap<String, Segment>,
    cached_value: OnceLock<Value>,
}

impl SegmentObject {
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
    pub fn to_segment(&self) -> Segment {
        let value = serde_json::to_value(self).unwrap_or(Value::Null);
        Segment::from_value(&value)
    }

    pub fn from_segment(seg: &Segment) -> Option<Self> {
        serde_json::from_value(seg.snapshot_to_value()).ok()
    }
}

// ================================
// Stream support
// ================================

#[derive(Debug, Clone)]
pub enum StreamEvent {
    Chunk(Segment),
    End(Segment),
    Error(String),
}

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

#[derive(Debug, Clone)]
pub struct SegmentStream {
    state: Arc<RwLock<StreamState>>,
    notify: Arc<Notify>,
    readers: Arc<AtomicUsize>,
}

#[derive(Debug, Clone)]
pub struct StreamWriter {
    state: Arc<RwLock<StreamState>>,
    notify: Arc<Notify>,
    readers: Arc<AtomicUsize>,
}

#[derive(Debug)]
pub struct StreamReader {
    stream: SegmentStream,
    cursor: usize,
}

#[derive(Debug, Clone, Default)]
pub struct StreamLimits {
    pub max_chunks: Option<usize>,
    pub max_buffer_bytes: Option<usize>,
}

impl SegmentStream {
    pub fn channel() -> (SegmentStream, StreamWriter) {
        Self::channel_with_limits(StreamLimits::default())
    }

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
        let stream = SegmentStream {
            state: shared.clone(),
            notify: notify.clone(),
            readers: readers.clone(),
        };
        let writer = StreamWriter {
            state: shared,
            notify,
            readers,
        };
        (stream, writer)
    }

    pub fn reader(&self) -> StreamReader {
        self.readers.fetch_add(1, Ordering::Relaxed);
        StreamReader {
            stream: self.clone(),
            cursor: 0,
        }
    }

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

    pub fn status(&self) -> StreamStatus {
        self.state
            .try_read()
            .map(|s| s.status.clone())
            .unwrap_or(StreamStatus::Running)
    }

    pub async fn status_async(&self) -> StreamStatus {
        self.state.read().await.status.clone()
    }

    pub fn chunks(&self) -> Vec<Segment> {
        self.state
            .try_read()
            .map(|s| s.chunks.items.clone())
            .unwrap_or_default()
    }

    pub async fn chunks_async(&self) -> Vec<Segment> {
        self.state.read().await.chunks.items.clone()
    }

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

    /// Convert Segment → serde_json::Value (non-stream only).
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
            Segment::Stream(_) => panic!(
                "Cannot call to_value() on Stream. Use snapshot_to_value() for explicit lossy conversion."
            ),
        }
    }

    /// Convert Segment → serde_json::Value (non-stream only), consuming self.
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
            Segment::Stream(_) => panic!(
                "Cannot call into_value() on Stream. Use snapshot_to_value() for explicit lossy conversion."
            ),
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

    pub fn is_none(&self) -> bool {
        matches!(self, Segment::None)
    }

    pub fn as_string(&self) -> Option<String> {
        match self {
            Segment::String(s) => Some(s.clone()),
            Segment::Integer(i) => Some(i.to_string()),
            Segment::Float(f) => Some(f.to_string()),
            Segment::Boolean(b) => Some(b.to_string()),
            _ => None,
        }
    }

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

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Segment::Integer(i) => Some(*i as f64),
            Segment::Float(f) => Some(*f),
            Segment::String(s) => s.parse::<f64>().ok(),
            _ => None,
        }
    }

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

#[derive(Debug, Clone)]
pub struct VariablePool {
    variables: ImHashMap<CompactString, Segment>,
    #[cfg(feature = "security")]
    selector_validation: Option<crate::security::validation::SelectorValidation>,
    #[cfg(not(feature = "security"))]
    selector_validation: Option<()>,
}

impl VariablePool {
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

    pub fn len(&self) -> usize {
        self.variables.len()
    }

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
        assert!(matches!(seg, Segment::Array(_)));

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
}
