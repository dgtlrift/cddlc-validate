//! Canonical decoded-data model shared by the JSON and CBOR validation paths.
//!
//! CBOR's data model is a strict superset of JSON's (RFC 8610 Appendix E), so
//! both formats decode into this one enum and [`crate::engine`] only has to
//! walk one representation instead of duplicating logic per format.

/// A decoded JSON or CBOR data item.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// Non-negative integer.
    Uint(u64),
    /// Negative integer (or an integer read from a source that doesn't
    /// distinguish sign, e.g. JSON, and happened to be negative).
    Int(i64),
    Float(f64),
    Text(String),
    /// Native CBOR byte string. JSON has no byte-string type; JSON input
    /// represents `bstr` fields as base64url text instead (see
    /// `crate::engine`'s primitive check, and RFC 8949 §6.1 — the same
    /// convention this project's own JSON codegen backends use).
    Bytes(Vec<u8>),
    Array(Vec<Value>),
    /// Key/value pairs in source order. JSON object keys are always
    /// [`Value::Text`]; CBOR map keys may be any `Value`.
    Map(Vec<(Value, Value)>),
    /// A CBOR tag (`#6.NN(...)`). Never produced from JSON input — JSON has
    /// no tag concept, so tagged types degrade to their inner type when
    /// validating JSON (see `crate::engine`).
    Tag(u64, Box<Value>),
}

impl From<serde_json::Value> for Value {
    fn from(v: serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(b),
            serde_json::Value::Number(n) => {
                if let Some(u) = n.as_u64() {
                    Value::Uint(u)
                } else if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else {
                    Value::Float(n.as_f64().unwrap_or(f64::NAN))
                }
            }
            serde_json::Value::String(s) => Value::Text(s),
            serde_json::Value::Array(a) => Value::Array(a.into_iter().map(Value::from).collect()),
            serde_json::Value::Object(o) => Value::Map(
                o.into_iter().map(|(k, v)| (Value::Text(k), Value::from(v))).collect(),
            ),
        }
    }
}

impl From<ciborium::Value> for Value {
    fn from(v: ciborium::Value) -> Self {
        match v {
            ciborium::Value::Integer(i) => integer_to_value(i.into()),
            ciborium::Value::Bytes(b) => Value::Bytes(b),
            ciborium::Value::Float(f) => Value::Float(f),
            ciborium::Value::Text(s) => Value::Text(s),
            ciborium::Value::Bool(b) => Value::Bool(b),
            ciborium::Value::Null => Value::Null,
            ciborium::Value::Tag(t, inner) => Value::Tag(t, Box::new(Value::from(*inner))),
            ciborium::Value::Array(a) => Value::Array(a.into_iter().map(Value::from).collect()),
            ciborium::Value::Map(m) => Value::Map(
                m.into_iter().map(|(k, v)| (Value::from(k), Value::from(v))).collect(),
            ),
            // `ciborium::Value` is #[non_exhaustive]; any future variant we
            // don't know about degrades to Null rather than failing to build.
            _ => Value::Null,
        }
    }
}

fn integer_to_value(n: i128) -> Value {
    if let Ok(u) = u64::try_from(n) {
        Value::Uint(u)
    } else if let Ok(i) = i64::try_from(n) {
        Value::Int(i)
    } else {
        // Beyond u64/i64 range (CBOR bignum-sized integer) — represent
        // approximately rather than panicking. The IR's own LiteralValue
        // has the same u64/i64/f64 ceiling, so no constraint can be checked
        // exactly against a value this large anyway.
        Value::Float(n as f64)
    }
}
