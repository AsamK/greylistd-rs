use crate::config::Config;
use anyhow::anyhow;
use crossbeam_channel::Receiver;
use serde::{Deserialize, Serialize};
use serde_ini::{from_read, to_writer};
use serde_plain::{derive_deserialize_from_fromstr, derive_serialize_from_display};
use serde_utils::{deserialize_systemtime_seconds, serialize_systemtime_seconds};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::fs::{exists, File};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{BufWriter, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::ops::Add;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::str::FromStr;
use std::sync::RwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub mod config;
pub mod serde_utils;

pub struct App {
    config: Config,
    triplets: HashMap<u64, GreylistEntry>,
    statistics: StoredStatistics,
}

impl App {
    pub fn new(config: Config) -> Result<App, anyhow::Error> {
        if !config.data.savetriplets {
            return Err(anyhow!("Option savetriplets must be enabled"));
        }
        if config.data.singleupdate || config.data.singlecheck {
            return Err(anyhow!(
                "Options singleupdate and singlecheck aren't supported yet"
            ));
        }
        let (triplets, statistics) =
            load_triplet_states(&config.data.tripletfile, &config.data.statefile)?;

        let only_subnet = config.data.onlysubnet;
        Ok(App {
            config,
            triplets: triplets
                .into_iter()
                .map(|entry| (entry.triplet.hash(only_subnet), entry))
                .collect(),
            statistics,
        })
    }

    pub fn run(
        mut self,
        listener: UnixListener,
        stop_signal: Receiver<()>,
    ) -> Result<bool, anyhow::Error> {
        use crossbeam_channel::{select, unbounded};

        let mut reload = false;
        let should_exit = RwLock::new(false);
        let (stream_sender, stream_receiver) = unbounded();
        std::thread::scope(|s| -> Result<(), anyhow::Error> {
            s.spawn(|| {
                for stream in listener.incoming() {
                    if *should_exit.read().unwrap() {
                        return;
                    }
                    if stream_sender.send(stream.unwrap()).is_err() {
                        return;
                    }
                }
            });

            loop {
                select! {
                    recv(stream_receiver) -> stream => {
                        match self.handle_client(stream?) {
                            Err(e) => eprintln!("Failed to handle request: {:?}", e),
                            Ok(result) => {
                                if result {
                        *should_exit.write().unwrap() = true;
                                    reload = true;
                                    break;
                                }
                            }
                        }
                    },
                    recv(stop_signal) -> _ => {
                        *should_exit.write().unwrap() = true;
                        break
                    },
                }

                let last_save = self.statistics.lastsave;
                let diff = SystemTime::now().duration_since(last_save)?;
                if diff > self.config.data.update {
                    self.save()?;
                }
            }
            self.save()?;
            // connect to socket to trigger exit
            UnixStream::connect(&self.config.socket.path)?;
            Ok(())
        })?;
        Ok(reload)
    }

    fn prune_expired_entries(&mut self) {
        let now = SystemTime::now();
        let oldest_retry = now - self.config.timeouts.retry_max;
        let oldest_expire = now - self.config.timeouts.expire;
        self.triplets.retain(|_, entry| match entry.listing_status {
            ListingStatus::Grey => entry.triplet_status.first_seen > oldest_retry,
            ListingStatus::White | ListingStatus::Black => {
                entry.triplet_status.last_seen > oldest_expire
            }
        });
    }

    fn save(&mut self) -> Result<(), anyhow::Error> {
        self.prune_expired_entries();
        let now = SystemTime::now();
        let triplets = self
            .triplets
            .iter()
            .map(|(hash, entry)| (hash.to_string(), &entry.triplet))
            .collect::<HashMap<_, _>>();

        let white = self
            .triplets
            .iter()
            .filter(|(_, entry)| entry.listing_status == ListingStatus::White)
            .map(|(hash, entry)| (hash.to_string(), entry.triplet_status.clone()))
            .collect::<HashMap<_, _>>();
        let grey = self
            .triplets
            .iter()
            .filter(|(_, entry)| entry.listing_status == ListingStatus::Grey)
            .map(|(hash, entry)| (hash.to_string(), entry.triplet_status.clone()))
            .collect::<HashMap<_, _>>();
        let black = self
            .triplets
            .iter()
            .filter(|(_, entry)| entry.listing_status == ListingStatus::Black)
            .map(|(hash, entry)| (hash.to_string(), entry.triplet_status.clone()))
            .collect::<HashMap<_, _>>();
        self.statistics.lastsave = now;
        let state = StoredStates {
            statistics: self.statistics.clone(),
            white,
            grey,
            black,
        };

        let triplet_file = File::create(&self.config.data.tripletfile)?;
        let state_file = File::create(&self.config.data.statefile)?;
        to_writer(triplet_file, &triplets)?;
        to_writer(state_file, &state)?;

        Ok(())
    }

    fn handle_client(&mut self, mut stream: UnixStream) -> Result<bool, anyhow::Error> {
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut buf = vec![0; 16384];
        let n = stream.read(&mut buf)?;
        buf.truncate(n);
        let line = String::from_utf8(buf)?;
        // let Some(line) = BufReader::new(stream.try_clone()?).lines().next() else {
        //     return Err(anyhow!("Empty request"));
        // };
        let cmd = line.parse::<Command>();
        let mut writer = BufWriter::new(stream);
        if let Ok(cmd) = cmd {
            match cmd {
                Command::Update {
                    triplet,
                    check_status,
                } => {
                    let entry = self.add_or_update_triplet(triplet);
                    if let Some(status) = check_status {
                        if entry.listing_status == status {
                            write!(writer, "true")?;
                        } else {
                            write!(writer, "false")?;
                        }
                    } else {
                        write!(writer, "{}", entry.listing_status)?;
                    }
                }
                Command::Save => {
                    self.save()?;
                    write!(writer, "greylistd data has been saved")?;
                }
                Command::Check {
                    triplet,
                    check_status,
                } => {
                    let status = self.check_triplet(triplet);
                    if let Some(check_status) = check_status {
                        if status == check_status {
                            write!(writer, "true")?;
                        } else {
                            write!(writer, "false")?;
                        }
                    } else {
                        write!(writer, "{}", status)?;
                    }
                }
                Command::Add {
                    triplet,
                    add_status,
                } => {
                    self.add_triplet(triplet, add_status.clone());
                    write!(writer, "Added to {}list", add_status)?;
                }
                Command::List { status } => {
                    let status = if status.is_empty() {
                        &[
                            ListingStatus::White,
                            ListingStatus::Grey,
                            ListingStatus::Black,
                        ][..]
                    } else {
                        &status
                    };
                    for list_status in status {
                        writeln!(writer, "{}list data:", list_status)?;
                        writeln!(writer, "=============")?;
                        writeln!(writer, "Last Seen            Count      Data")?;
                        for entry in self.triplets.values() {
                            if entry.listing_status != *list_status {
                                continue;
                            }
                            writeln!(
                                writer,
                                "{: <20?} {: <10} {}",
                                entry
                                    .triplet_status
                                    .last_seen
                                    .duration_since(UNIX_EPOCH)
                                    .unwrap()
                                    .as_secs(),
                                entry.triplet_status.count,
                                entry.triplet
                            )?;
                        }
                        writeln!(writer)?
                    }
                }
                Command::Delete { triplet } => {
                    let entry = self.triplets.remove(&self.hash_triplet(&triplet));
                    if let Some(entry) = entry {
                        write!(writer, "Removed from {}list", entry.listing_status)?;
                    } else {
                        write!(writer, "Not found")?;
                    }
                }
                Command::Clear { status } => {
                    if status.is_empty() {
                        self.triplets.drain();
                        self.statistics = Default::default();
                    } else {
                        self.triplets
                            .retain(|_, v| !status.contains(&v.listing_status))
                    }
                    write!(writer, "data and statistics cleared")?;
                }
                Command::Reload => {
                    write!(writer, "reloading configuration and data")?;
                    return Ok(true);
                }
                Command::Status { triplet } => {
                    if let Some(entry) = self.get_entry(&triplet) {
                        write!(writer, "{}", entry.listing_status)?;
                    } else {
                        write!(writer, "unseen")?;
                    };
                }
                Command::Stats => {
                    writeln!(
                        writer,
                        "Statistics since {} ({}s ago)",
                        self.statistics
                            .start
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_secs(),
                        SystemTime::now()
                            .duration_since(self.statistics.start)
                            .unwrap()
                            .as_secs(),
                    )?;
                    writeln!(writer)?;
                    for state in [
                        ListingStatus::White,
                        ListingStatus::Grey,
                        ListingStatus::Black,
                    ] {
                        let (item_count, request_count) = self
                            .triplets
                            .iter()
                            .filter(|(_, e)| e.listing_status == state)
                            .fold((0, 0), |(item, req), (_, e)| {
                                (item + 1, req + e.triplet_status.count)
                            });
                        writeln!(
                            writer,
                            "{} items, matching {} requests, are currently {}listed",
                            item_count, request_count, state
                        )?;
                    }
                    writeln!(writer)?;

                    let grey_count = self
                        .triplets
                        .iter()
                        .filter(|(_, e)| e.listing_status == ListingStatus::Grey)
                        .count() as u32;
                    let previous_grey = self.statistics.grey - grey_count;
                    let expired_grey = previous_grey - self.statistics.white;

                    writeln!(
                        writer,
                        "Of {} items that were initially greylisted:",
                        previous_grey
                    )?;

                    writeln!(
                        writer,
                        " - {} ({:.1}%) became whitelisted",
                        self.statistics.white,
                        self.statistics.white as f64 * 100.0 / previous_grey as f64
                    )?;

                    writeln!(
                        writer,
                        " - {} ({:.1}%) expired from the greylist",
                        expired_grey,
                        expired_grey as f64 * 100.0 / previous_grey as f64
                    )?;
                }
                Command::Mrtg => {
                    self.prune_expired_entries();
                    writeln!(writer, "{}", self.statistics.grey)?;
                    writeln!(writer, "{}", self.statistics.white)?;
                    writeln!(
                        writer,
                        "{}",
                        SystemTime::now()
                            .duration_since(self.statistics.start)
                            .unwrap()
                            .as_secs()
                    )?;
                    writeln!(writer, "hostname")?;
                }
            }
        } else {
            write!(writer, "Invalid command")?;
        };
        Ok(false)
    }

    fn get_entry(&self, triplet: &Triplet) -> Option<&GreylistEntry> {
        let hash = self.hash_triplet(triplet);
        self.triplets.get(&hash)
    }

    fn hash_triplet(&self, triplet: &Triplet) -> u64 {
        triplet.hash(self.config.data.onlysubnet)
    }

    fn check_triplet(&self, triplet: Triplet) -> ListingStatus {
        let Some(entry) = self.get_entry(&triplet) else {
            return ListingStatus::Grey;
        };
        if entry.listing_status == ListingStatus::Grey {
            let now = SystemTime::now();
            let diff = now.duration_since(entry.triplet_status.first_seen).unwrap();
            if diff <= self.config.timeouts.retry_max && diff >= self.config.timeouts.retry_min {
                return ListingStatus::White;
            }
        }

        entry.listing_status.clone()
    }

    fn add_triplet(&mut self, triplet: Triplet, listing_status: ListingStatus) -> &GreylistEntry {
        let now = SystemTime::now();
        let hash = self.hash_triplet(&triplet);
        let entry = self
            .triplets
            .entry(hash)
            .and_modify(|entry| {
                entry.triplet_status.last_seen = now;
                entry.listing_status = listing_status.clone();
            })
            .or_insert_with(|| GreylistEntry {
                triplet,
                listing_status,
                triplet_status: TripletStatus {
                    first_seen: now,
                    last_seen: now,
                    count: 0,
                },
            });
        entry
    }

    fn add_or_update_triplet(&mut self, triplet: Triplet) -> &GreylistEntry {
        let now = SystemTime::now();
        let hash = self.hash_triplet(&triplet);
        let entry = self
            .triplets
            .entry(hash)
            .and_modify(|entry| {
                entry.triplet_status.last_seen = now;
                entry.triplet_status.count += 1;
                if let ListingStatus::Grey = entry.listing_status {
                    let diff = now.duration_since(entry.triplet_status.first_seen).unwrap();
                    if diff > self.config.timeouts.retry_max {
                        entry.triplet_status.first_seen = now;
                    } else if diff >= self.config.timeouts.retry_min {
                        self.statistics.white += 1;
                        entry.listing_status = ListingStatus::White;
                    }
                }
            })
            .or_insert_with(|| {
                self.statistics.grey += 1;
                GreylistEntry {
                    triplet,
                    listing_status: ListingStatus::Grey,
                    triplet_status: TripletStatus {
                        first_seen: now,
                        last_seen: now,
                        count: 1,
                    },
                }
            });
        entry
    }
}

#[derive(Debug)]
struct Triplet {
    sender_ip: IpAddr,
    sender_email: Option<String>,
    recipient_email: String,
}

impl Triplet {
    fn hash(&self, only_subnet: bool) -> u64 {
        let mut s = DefaultHasher::new();
        if !only_subnet {
            self.sender_ip.hash(&mut s);
        } else {
            let subnet_ip = match self.sender_ip {
                IpAddr::V4(ip) => {
                    let mut octets = ip.octets();
                    octets[3] = 0;
                    IpAddr::V4(Ipv4Addr::from(octets))
                }
                IpAddr::V6(ip) => {
                    let mut octets = ip.octets();
                    for octet in octets.iter_mut().skip(7) {
                        *octet = 0;
                    }
                    IpAddr::V6(Ipv6Addr::from(octets))
                }
            };
            subnet_ip.hash(&mut s);
        }
        self.sender_email.hash(&mut s);
        self.recipient_email.hash(&mut s);
        s.finish()
    }
}

impl FromStr for Triplet {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts = s.split(" ").collect::<Vec<_>>();
        if parts.len() == 2 {
            Ok(Triplet {
                sender_ip: IpAddr::from_str(parts.first().unwrap())?,
                sender_email: None,
                recipient_email: parts.get(1).unwrap().to_string(),
            })
        } else if parts.len() == 3 {
            Ok(Triplet {
                sender_ip: IpAddr::from_str(parts.first().unwrap())?,
                sender_email: Some(parts.get(1).unwrap().to_string()),
                recipient_email: parts.get(2).unwrap().to_string(),
            })
        } else {
            Err(anyhow!("Invalid triplet: {}", s))
        }
    }
}
derive_deserialize_from_fromstr!(Triplet, "Invalid triplet");

impl Display for Triplet {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if let Some(sender_email) = &self.sender_email {
            f.write_fmt(format_args!(
                "{} {} {}",
                self.sender_ip, sender_email, self.recipient_email,
            ))
        } else {
            f.write_fmt(format_args!("{} {}", self.sender_ip, self.recipient_email,))
        }
    }
}
derive_serialize_from_display!(Triplet);

#[derive(Clone, Debug)]
struct TripletStatus {
    last_seen: SystemTime,
    first_seen: SystemTime,
    count: u32,
}

impl FromStr for TripletStatus {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts = s.split(" ").collect::<Vec<_>>();
        if parts.len() == 3 {
            Ok(TripletStatus {
                last_seen: UNIX_EPOCH.add(Duration::from_secs(parts.first().unwrap().parse()?)),
                first_seen: UNIX_EPOCH.add(Duration::from_secs(parts.get(1).unwrap().parse()?)),
                count: parts.get(2).unwrap().parse()?,
            })
        } else {
            Err(anyhow!("Invalid triplet status: {}", s))
        }
    }
}
derive_deserialize_from_fromstr!(TripletStatus, "Invalid status");

impl Display for TripletStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "{} {} {}",
            self.last_seen.duration_since(UNIX_EPOCH).unwrap().as_secs(),
            self.first_seen
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            self.count,
        ))
    }
}
derive_serialize_from_display!(TripletStatus);

#[derive(Debug)]
pub struct GreylistEntry {
    triplet: Triplet,
    triplet_status: TripletStatus,
    listing_status: ListingStatus,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct StoredStatistics {
    white: u32,
    grey: u32,
    black: u32,
    #[serde(
        deserialize_with = "deserialize_systemtime_seconds",
        serialize_with = "serialize_systemtime_seconds"
    )]
    start: SystemTime,
    #[serde(
        deserialize_with = "deserialize_systemtime_seconds",
        serialize_with = "serialize_systemtime_seconds"
    )]
    lastsave: SystemTime,
}

impl Default for StoredStatistics {
    fn default() -> Self {
        Self {
            white: 0,
            grey: 0,
            black: 0,
            start: SystemTime::now(),
            lastsave: SystemTime::UNIX_EPOCH,
        }
    }
}

#[derive(Default, Deserialize, Serialize)]
struct StoredStates {
    white: HashMap<String, TripletStatus>,
    grey: HashMap<String, TripletStatus>,
    black: HashMap<String, TripletStatus>,
    statistics: StoredStatistics,
}

pub fn load_triplet_states(
    file_triplets: impl AsRef<Path>,
    file_states: impl AsRef<Path>,
) -> Result<(Vec<GreylistEntry>, StoredStatistics), anyhow::Error> {
    let triplets = if !exists(&file_triplets)? {
        Default::default()
    } else {
        from_read::<_, HashMap<String, Triplet>>(File::open(file_triplets)?)?
    };
    let mut states = if !exists(&file_states)? {
        Default::default()
    } else {
        from_read::<_, StoredStates>(File::open(file_states)?)?
    };
    let entries = triplets
        .into_iter()
        .map(|(hash, triplet)| {
            let (listing_status, triplet_status) = if let Some(state) = states.white.remove(&hash) {
                (ListingStatus::White, state)
            } else if let Some(state) = states.grey.remove(&hash) {
                (ListingStatus::Grey, state)
            } else if let Some(state) = states.black.remove(&hash) {
                (ListingStatus::Black, state)
            } else {
                return Err(anyhow!("Triplet status not found: {}", triplet));
            };
            Ok(GreylistEntry {
                triplet,
                triplet_status,
                listing_status,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok((entries, states.statistics))
}

#[derive(Debug)]
enum Command {
    Add {
        triplet: Triplet,
        add_status: ListingStatus,
    },
    Delete {
        triplet: Triplet,
    },
    Check {
        triplet: Triplet,
        check_status: Option<ListingStatus>,
    },
    Update {
        triplet: Triplet,
        check_status: Option<ListingStatus>,
    },
    Stats,
    Status {
        triplet: Triplet,
    },
    Mrtg,
    List {
        status: Vec<ListingStatus>,
    },
    Save,
    Reload,
    Clear {
        status: Vec<ListingStatus>,
    },
}

fn parse_cmd_input(mut input: &str) -> Result<(Vec<&str>, &str), anyhow::Error> {
    let mut args = Vec::new();
    while input.starts_with("--") {
        let (arg, rest) = input.split_once(" ").ok_or(anyhow!("Invalid command"))?;
        args.push(arg);
        input = rest;
    }
    Ok((args, input))
}

impl FromStr for Command {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts = s.split_once(" ").unwrap_or((s, ""));
        let cmd = match parts.0 {
            "add" => {
                let (args, rest) = parse_cmd_input(parts.1)?;
                let mut add_status = None;
                for arg in args {
                    let status = status_from_arg(arg);
                    if let Some(status) = status {
                        add_status = Some(status)
                    }
                }
                let triplet = rest.parse()?;
                Command::Add {
                    triplet,
                    add_status: add_status.unwrap_or(ListingStatus::White),
                }
            }
            "delete" => {
                let (_, rest) = parse_cmd_input(parts.1)?;
                let triplet = rest.parse()?;
                Command::Delete { triplet }
            }
            "check" => {
                let (args, rest) = parse_cmd_input(parts.1)?;
                let mut check_status = None;
                for arg in args {
                    let status = status_from_arg(arg);
                    if let Some(status) = status {
                        check_status = Some(status)
                    }
                }
                let triplet = rest.parse()?;
                Command::Check {
                    triplet,
                    check_status,
                }
            }
            "stats" => Command::Stats,
            "status" => {
                let (_, rest) = parse_cmd_input(parts.1)?;
                let triplet = rest.parse()?;
                Command::Status { triplet }
            }
            "mrtg" => Command::Mrtg,
            "list" => {
                let (args, _) = parse_cmd_input(parts.1)?;
                let mut status_list = Vec::new();
                for arg in args {
                    let status = status_from_arg(arg);
                    if let Some(status) = status {
                        status_list.push(status);
                    }
                }
                Command::List {
                    status: status_list,
                }
            }
            "save" => Command::Save,
            "clear" => {
                let (args, _) = parse_cmd_input(parts.1)?;
                let mut status_list = Vec::new();
                for arg in args {
                    let status = status_from_arg(arg);
                    if let Some(status) = status {
                        status_list.push(status);
                    }
                }
                Command::Clear {
                    status: status_list,
                }
            }
            "reload" => Command::Reload,
            // "update" |
            _ => {
                let input = if parts.0 == "update" { parts.1 } else { s };
                let (args, rest) = parse_cmd_input(input)?;
                let mut check_status = None;
                for arg in args {
                    let status = status_from_arg(arg);
                    if let Some(status) = status {
                        check_status = Some(status)
                    }
                }
                let triplet = rest.parse()?;
                Command::Update {
                    triplet,
                    check_status,
                }
            }
        };
        Ok(cmd)
    }
}

derive_deserialize_from_fromstr!(Command, "Invalid command");

fn status_from_arg(arg: &str) -> Option<ListingStatus> {
    match arg {
        "--white" => Some(ListingStatus::White),
        "--grey" => Some(ListingStatus::Grey),
        "--black" => Some(ListingStatus::Black),
        _ => None,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ListingStatus {
    White,
    Grey,
    Black,
}

impl Display for ListingStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "{}",
            match self {
                ListingStatus::White => "white",
                ListingStatus::Grey => "grey",
                ListingStatus::Black => "black",
            }
        ))
    }
}
