use compact_str::CompactString;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Sentinel node ID used for scoped (local) variables within sub-graphs.
pub const SCOPE_NODE_ID: &str = "__scope__";

/// A two-part variable address: `(node_id, variable_name)`.
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
        CompactString::new(format!("{}:{}", self.node_id, self.variable_name))
    }

    pub fn from_pool_key(pool_key: &str) -> Option<Self> {
        let (node_id, variable_name) = pool_key
            .split_once(':')
            .or_else(|| pool_key.split_once('.'))?;
        if node_id.is_empty() || variable_name.is_empty() {
            return None;
        }
        Some(Self::new(node_id, variable_name))
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
