// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! JSON documents, for output other programs read.

use core::fmt::{self, Write as _};
use core::time::Duration;

/// A JSON value.
#[derive(Clone, Debug)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Unsigned(u64),
    Signed(i64),
    Float(f64),
    String(String),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
}

impl Json {
    /// An object of `fields`, in order.
    pub(crate) fn object<'a>(fields: impl IntoIterator<Item = (&'a str, Self)>) -> Self {
        Self::Object(
            fields
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        )
    }

    /// `duration` in whole nanoseconds.
    pub(crate) fn nanos(duration: Duration) -> Self {
        Self::Unsigned(u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX))
    }

    /// `duration` in whole nanoseconds, or null.
    pub(crate) fn nanos_or_null(duration: Option<Duration>) -> Self {
        duration.map_or(Self::Null, Self::nanos)
    }
}

impl From<bool> for Json {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<f64> for Json {
    fn from(value: f64) -> Self {
        if value.is_finite() {
            Self::Float(value)
        } else {
            Self::Null
        }
    }
}

impl From<u64> for Json {
    fn from(value: u64) -> Self {
        Self::Unsigned(value)
    }
}

impl From<u32> for Json {
    fn from(value: u32) -> Self {
        Self::Unsigned(value.into())
    }
}

impl From<&str> for Json {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<String> for Json {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl<T: Into<Self>> From<Option<T>> for Json {
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Null, Into::into)
    }
}

impl fmt::Display for Json {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("null"),
            Self::Bool(value) => write!(f, "{value}"),
            Self::Unsigned(value) => write!(f, "{value}"),
            Self::Signed(value) => write!(f, "{value}"),
            Self::Float(value) => write!(f, "{value}"),
            Self::String(value) => write_string(f, value),
            Self::Array(values) => {
                f.write_char('[')?;
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        f.write_char(',')?;
                    }
                    write!(f, "{value}")?;
                }
                f.write_char(']')
            }
            Self::Object(fields) => {
                f.write_char('{')?;
                for (index, (key, value)) in fields.iter().enumerate() {
                    if index > 0 {
                        f.write_char(',')?;
                    }
                    write_string(f, key)?;
                    write!(f, ":{value}")?;
                }
                f.write_char('}')
            }
        }
    }
}

fn write_string(f: &mut fmt::Formatter<'_>, value: &str) -> fmt::Result {
    f.write_char('"')?;
    for ch in value.chars() {
        match ch {
            '"' => f.write_str("\\\"")?,
            '\\' => f.write_str("\\\\")?,
            '\n' => f.write_str("\\n")?,
            '\r' => f.write_str("\\r")?,
            '\t' => f.write_str("\\t")?,
            ch if ch < '\u{20}' => write!(f, "\\u{:04x}", u32::from(ch))?,
            _ => f.write_char(ch)?,
        }
    }
    f.write_char('"')
}

#[cfg(test)]
mod tests {
    use super::Json;

    #[test]
    fn values_write_as_json() {
        let value = Json::object([
            ("name", Json::from("a \"quoted\"\nline\u{1}")),
            ("count", Json::from(3_u64)),
            ("offset", Json::Signed(-2)),
            ("none", Json::from(None::<u64>)),
            ("list", Json::Array(vec![Json::from(true), Json::Null])),
        ]);
        assert_eq!(
            value.to_string(),
            r#"{"name":"a \"quoted\"\nline\u0001","count":3,"offset":-2,"none":null,"list":[true,null]}"#
        );
    }
}
