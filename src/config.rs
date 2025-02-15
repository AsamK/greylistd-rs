use crate::serde_utils::{deserialize_bool, deserialize_duration_seconds};
use serde::Deserialize;
use serde_ini::from_read;
use std::fs::File;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Deserialize)]
pub struct Config {
    pub(crate) timeouts: Timeouts,
    pub socket: Socket,
    pub(crate) data: Data,
}

impl Config {
    pub fn load(file: &str) -> Result<Config, anyhow::Error> {
        let file = File::open(file)?;
        Ok(from_read::<_, Config>(file)?)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Timeouts {
    /// Initial delay before previously unknown triplets are allowed to pass
    /// Default is 10 minutes = 600 seconds
    #[serde(default = "_default_retry_min")]
    #[serde(deserialize_with = "deserialize_duration_seconds")]
    pub(crate) retry_min: Duration,

    /// Lifetime of triplets that have not been retried after initial delay
    /// Default is 8 hours = 28800 seconds
    #[serde(default = "_default_retry_max")]
    #[serde(deserialize_with = "deserialize_duration_seconds")]
    pub(crate) retry_max: Duration,

    /// Lifetime of auto-whitelisted triplets that have allowed mail to pass
    /// Default is 60 days = 5,184,000 seconds
    #[serde(default = "_default_expire")]
    #[serde(deserialize_with = "deserialize_duration_seconds")]
    pub(crate) expire: Duration,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Socket {
    /// Path to the UNIX domain socket on which greylistd will listen.
    /// The parent directory must be writable by the user running 'greylistd'.
    /// Default path is "/var/run/greylistd/socket".
    pub path: PathBuf,

    /// UNIX filemode of that socket.  See "chmod(1)" for the meaning of this.
    /// Default mode is 0660.
    pub mode: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Data {
    /// Update interval -- save data to the filesystem if it has been more
    /// than this many seconds (default 600) since the last save.
    #[serde(default = "_default_update")]
    #[serde(deserialize_with = "deserialize_duration_seconds")]
    pub(crate) update: Duration,

    /// Path to the file containing the current state of each data item (triplet),
    /// along with some general statistics.
    /// Default is "/var/lib/greylistd/states".
    #[serde(default = "_default_statefile")]
    pub(crate) statefile: PathBuf,

    /// Path to the file that will contain the original, unhashed data for the
    /// "list" command.
    /// Default is "/var/lib/greylistd/triplets".
    #[serde(default = "_default_tripletfile")]
    pub(crate) tripletfile: PathBuf,

    /// Whether or not to retain unhashed triplets, for the "list" command.
    /// Default is "true"
    #[serde(default = "_default_true")]
    #[serde(deserialize_with = "deserialize_bool")]
    pub(crate) savetriplets: bool,

    /// Whether check/update also checks for a whitelist entry, which only
    /// contains the first word of the triplet, that is the IP address usually.
    /// If set to true, you can also insert general IP addresses/networks into the
    /// whitelist, without email addresses.
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_bool")]
    pub(crate) singlecheck: bool,

    /// Whether update only inserts the first word of the triplet into the
    /// whitelist, that is the IP address usually. Meant to be used in
    /// conjunction with singlecheck = true.
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_bool")]
    pub(crate) singleupdate: bool,

    /// Whether the complete IP should be checked, or only the subnet (/24 for IPv4 and /64 for IPv6)
    #[serde(default = "_default_true")]
    #[serde(deserialize_with = "deserialize_bool")]
    pub(crate) onlysubnet: bool,
}

const fn _default_true() -> bool {
    true
}

fn _default_retry_min() -> Duration {
    Duration::from_secs(600)
}

fn _default_retry_max() -> Duration {
    Duration::from_secs(28800)
}

fn _default_expire() -> Duration {
    Duration::from_secs(5184000)
}

fn _default_update() -> Duration {
    Duration::from_secs(600)
}

fn _default_statefile() -> PathBuf {
    "/var/lib/greylistd/states".into()
}

fn _default_tripletfile() -> PathBuf {
    "/var/lib/greylistd/triplets".into()
}
