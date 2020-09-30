use bincode::Options;
use log::info;
use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value as SerdeValue;
use std::fmt;
use std::ops::Deref;
use traits::{EventMapKey, SharedEventMap};

pub mod build_helper;
pub mod core;
pub mod device_info;
pub mod frame;
pub mod object_tracker;
pub mod remotable;
pub mod remote_service;
pub mod remote_services_registrar;
mod selinux;
pub mod socket_pair;
pub mod traits;

pub use bincode::Error as BincodeError;

pub fn get_bincode() -> impl Options {
    bincode::options().with_big_endian().with_varint_encoding()
}

pub fn deserialize_bincode<'a, T>(input: &'a [u8]) -> Result<T, BincodeError>
where
    T: Deserialize<'a>,
{
    get_bincode().deserialize(input)
}

pub fn is_event_in_map(map: &SharedEventMap, service: u32, object: u32, event: u32) -> bool {
    let res = match map.lock().get(&EventMapKey::new(service, object, event)) {
        Some(&true) => true,
        Some(&false) => false,
        None => false,
    };

    info!(
        "Checking event service #{} object #{} event #{} : {}",
        service, object, event, res
    );

    res
}

// A wrapper around a JsonValue to help with the encoding/decoding of JSON strings.
#[derive(Clone, Debug)]
pub struct JsonValue(SerdeValue);

impl<'de> Deserialize<'de> for JsonValue {
    fn deserialize<D>(deserializer: D) -> Result<JsonValue, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error;
        use std::str::FromStr;

        struct JsonVisitor;
        impl<'de> Visitor<'de> for JsonVisitor {
            type Value = String;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                write!(formatter, "a JSON string")
            }

            fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(s.to_owned())
            }
        }

        let json_str = deserializer.deserialize_str(JsonVisitor)?;
        let value =
            SerdeValue::from_str(&json_str).map_err(|err| D::Error::custom(format!("{}", err)))?;

        Ok(JsonValue(value))
    }
}

impl Serialize for JsonValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl Deref for JsonValue {
    type Target = SerdeValue;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<SerdeValue> for JsonValue {
    fn from(v: SerdeValue) -> Self {
        JsonValue(v)
    }
}

// A wrapper around a std::time::SystemTime to provide serde support as u64 milliseconds since epoch.
#[derive(Clone, Debug)]
pub struct SystemTime(std::time::SystemTime);

impl<'de> Deserialize<'de> for SystemTime {
    fn deserialize<D>(deserializer: D) -> Result<SystemTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct TimeVisitor;
        impl<'de> Visitor<'de> for TimeVisitor {
            type Value = u64;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                write!(formatter, "u64: time in ms since eopch")
            }

            fn visit_u64<E>(self, val: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(val)
            }
        }

        let val = deserializer.deserialize_u64(TimeVisitor)?;
        log::debug!("Got Date as u64={}", val);
        let time = std::time::UNIX_EPOCH;
        Ok(SystemTime(
            time.checked_add(std::time::Duration::from_millis(val))
                .unwrap(),
        ))
    }
}

impl Serialize for SystemTime {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if let Ok(from_epoch) = self.0.duration_since(std::time::UNIX_EPOCH) {
            serializer.serialize_u64(from_epoch.as_millis() as _)
        } else {
            serializer.serialize_u64(0)
        }
    }
}

impl Deref for SystemTime {
    type Target = std::time::SystemTime;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<std::time::SystemTime> for SystemTime {
    fn from(v: std::time::SystemTime) -> Self {
        SystemTime(v)
    }
}