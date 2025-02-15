use serde::{de, ser};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn deserialize_systemtime_seconds<'de, D>(deserializer: D) -> Result<SystemTime, D::Error>
where
    D: de::Deserializer<'de>,
{
    let s: u64 = de::Deserialize::deserialize(deserializer)?;

    Ok(UNIX_EPOCH + Duration::from_secs(s))
}

pub fn serialize_systemtime_seconds<S>(value: &SystemTime, serializer: S) -> Result<S::Ok, S::Error>
where
    S: ser::Serializer,
{
    serializer.serialize_u64(value.duration_since(UNIX_EPOCH).unwrap().as_secs())
}

pub fn deserialize_duration_seconds<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: de::Deserializer<'de>,
{
    let s: u64 = de::Deserialize::deserialize(deserializer)?;

    Ok(Duration::from_secs(s))
}

pub fn deserialize_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: de::Deserializer<'de>,
{
    let s: String = de::Deserialize::deserialize(deserializer)?;

    bool::from_str(&s).map_err(|_| de::Error::unknown_variant(&s, &["true", "false"]))
}
