// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a guest is to run, as its driver writes it on the config drive: a magic, the length of
//! what follows, and the config as JSON. The drive is [`SIZE`] bytes; its last block is the
//! guest's to write ([`crate::status`]), so a config never reaches it.

use crate::Error;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::IpAddr;

/// The config drive's size.
pub const SIZE: u64 = 1 << 20;

/// What the drive begins with.
const MAGIC: &[u8; 8] = b"KORPISG1";

/// The magic and the length.
const HEADER: usize = 12;

/// What a guest is to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The guest's hostname.
    pub hostname: String,
    /// The entry: its program, then its arguments. The program is looked up on the entry's own
    /// `PATH` when it is not a path.
    pub argv: Vec<String>,
    /// The entry's whole environment.
    pub env: BTreeMap<String, String>,
    /// Where the entry starts; made when the image lacks it.
    pub working_dir: String,
    /// The user the entry runs as, a number: the image's own names are not read.
    pub uid: u32,
    /// Its group.
    pub gid: u32,
    /// The guest's network, absent for one with loopback alone.
    pub network: Option<Network>,
    /// The volumes, each on the drive of its place in this list ([`crate::drive::volume`]).
    pub volumes: Vec<Volume>,
    /// The console takes input: the entry runs with the serial port as its controlling
    /// terminal, as a container's interactive console runs under one. Otherwise its input is
    /// empty and its output reaches the console as it wrote it.
    #[serde(default)]
    pub interactive: bool,
}

/// The guest's one interface, `eth0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Network {
    /// Its addresses, each with its prefix length.
    pub addresses: Vec<(IpAddr, u8)>,
    /// A default route through each, taken to be on the link.
    pub gateways: Vec<IpAddr>,
    /// The resolvers `/etc/resolv.conf` names, in place of what the image holds there; none
    /// leaves the image's.
    #[serde(default)]
    pub resolvers: Vec<IpAddr>,
}

/// A volume: its drive's filesystem, mounted at `path`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Volume {
    /// Where the entry sees it.
    pub path: String,
    /// Mounted read-only.
    pub read_only: bool,
}

impl Config {
    /// Writes the config at the start of `drive`, and makes the drive [`SIZE`] bytes long.
    pub fn write(&self, drive: &mut std::fs::File) -> Result<(), Error> {
        let json = serde_json::to_vec(self)?;
        let len = u32::try_from(json.len()).map_err(|_| Error::TooLarge(json.len()))?;
        if u64::from(len) + u64::try_from(HEADER).unwrap_or(u64::MAX) > crate::status::OFFSET {
            return Err(Error::TooLarge(json.len()));
        }
        drive.set_len(SIZE)?;
        drive.seek(SeekFrom::Start(0))?;
        drive.write_all(MAGIC)?;
        drive.write_all(&len.to_le_bytes())?;
        drive.write_all(&json)?;
        drive.sync_all()?;
        Ok(())
    }

    /// Reads the config `drive` begins with.
    pub fn read(drive: &mut impl Read) -> Result<Self, Error> {
        let mut header = [0; HEADER];
        drive.read_exact(&mut header)?;
        let (magic, len) = header.split_at(MAGIC.len());
        if magic != MAGIC {
            return Err(Error::NotConfig);
        }
        let len = u32::from_le_bytes(len.try_into().map_err(|_| Error::NotConfig)?);
        let len = usize::try_from(len).map_err(|_| Error::NotConfig)?;
        if u64::try_from(len + HEADER).unwrap_or(u64::MAX) > crate::status::OFFSET {
            return Err(Error::TooLarge(len));
        }
        let mut json = vec![0; len];
        drive.read_exact(&mut json)?;
        Ok(serde_json::from_slice(&json)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sleeper() -> Config {
        Config {
            hostname: "w".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), "exec sleep 9".into()],
            env: BTreeMap::from([("PATH".into(), "/bin".into())]),
            working_dir: "/data".into(),
            uid: 1000,
            gid: 1000,
            network: Some(Network {
                addresses: vec![("10.200.0.2".parse().unwrap(), 32)],
                gateways: vec!["169.254.1.1".parse().unwrap()],
                resolvers: vec!["169.254.1.53".parse().unwrap()],
            }),
            volumes: vec![Volume { path: "/data".into(), read_only: false }],
            interactive: true,
        }
    }

    #[test]
    fn a_config_is_read_back_as_it_was_written() {
        let mut drive = tempfile::tempfile().unwrap();
        sleeper().write(&mut drive).unwrap();
        assert_eq!(drive.metadata().unwrap().len(), SIZE);
        drive.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(Config::read(&mut drive).unwrap(), sleeper());
    }

    #[test]
    fn a_drive_of_another_format_or_a_config_too_large_is_refused() {
        let mut zeros = std::io::Cursor::new(vec![0; 64]);
        assert!(matches!(Config::read(&mut zeros), Err(Error::NotConfig)));
        let mut huge = sleeper();
        huge.env.insert("BIG".into(), "x".repeat(1 << 20));
        let mut drive = tempfile::tempfile().unwrap();
        assert!(matches!(huge.write(&mut drive), Err(Error::TooLarge(_))));
        let mut lying = MAGIC.to_vec();
        lying.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(Config::read(&mut lying.as_slice()), Err(Error::TooLarge(_))));
    }
}
