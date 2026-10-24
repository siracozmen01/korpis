// SPDX-License-Identifier: Apache-2.0

//! What the suite is given: the driver, by how it is started and the socket it serves on, and the
//! node it would run on, by the paths the driver is started with (§8 of docs/04-runtimes.md). The
//! suite writes into the content store and the volume root as the agent and the helper would, and
//! the driver only reads them.

use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::PathBuf;

/// How many ids each object is given unless the fixture says, as a node gives a workload.
pub const IDS: u32 = 65_536;
/// How many objects at most the suite makes unless the fixture says.
pub const OBJECTS: u32 = 64;

/// A program, and the arguments it is started with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Launch {
    /// The program.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<OsString>,
}

/// The driver under test and the node it serves.
#[derive(Debug, Clone)]
pub struct Fixture {
    /// How the driver is started: the suite starts it, kills it as a crash would, and starts it
    /// again, so it serves on [`Fixture::socket`] with the paths below each time.
    pub driver: Launch,
    /// The driver's socket, on which it serves `korpis.driver.v1`.
    pub socket: PathBuf,
    /// What a restart of the host does to the driver's objects, run once the driver has been
    /// killed with the process id of each object as a further argument, before the driver is
    /// started again; without one, the case of a restarted host is not run. It ends what a host
    /// restart ends: each object's processes and whatever the runtime keeps beside them.
    pub host_restart: Option<Launch>,
    /// The node's content store, as the driver was started with it: blobs by digest under
    /// `blobs/sha256/`.
    pub content: PathBuf,
    /// The directory volumes are kept under, as the driver was started with it.
    pub volume_root: PathBuf,
    /// The probe program (`korpis-conformance-probe`), built static for this machine.
    pub probe: PathBuf,
    /// The first id of the ranges the suite's objects run as, a range of [`Fixture::uid_count`]
    /// each, above any a user of the machine holds.
    pub uid_base: u32,
    /// How many ids each object is given.
    pub uid_count: u32,
    /// How many objects at most the suite makes, which bounds the ids it takes.
    pub objects: u32,
}

/// A program as a fixture file names it: arguments are text, as JSON's strings are.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchFile {
    program: PathBuf,
    #[serde(default)]
    args: Vec<String>,
}

/// A fixture as a file holds it, in JSON, for the runner a third party starts (§8 of
/// docs/04-runtimes.md): each field of [`Fixture`] by its name, `host_restart` left out where
/// there is none, and `uid_count` and `objects` left out for [`IDS`] and [`OBJECTS`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureFile {
    driver: LaunchFile,
    socket: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host_restart: Option<LaunchFile>,
    content: PathBuf,
    volume_root: PathBuf,
    probe: PathBuf,
    uid_base: u32,
    #[serde(default = "ids")]
    uid_count: u32,
    #[serde(default = "objects")]
    objects: u32,
}

const fn ids() -> u32 {
    IDS
}

const fn objects() -> u32 {
    OBJECTS
}

impl From<LaunchFile> for Launch {
    fn from(l: LaunchFile) -> Self {
        Self { program: l.program, args: l.args.into_iter().map(OsString::from).collect() }
    }
}

impl TryFrom<&Launch> for LaunchFile {
    type Error = String;

    fn try_from(l: &Launch) -> Result<Self, String> {
        let args = l.args.iter().map(|a| {
            a.to_str().map(str::to_owned).ok_or_else(|| format!("{} is not text", a.display()))
        });
        Ok(Self { program: l.program.clone(), args: args.collect::<Result<_, _>>()? })
    }
}

impl Fixture {
    /// The fixture a file holds, in JSON.
    ///
    /// # Errors
    /// It is not JSON, or not a fixture: a field missing, unknown, or of the wrong kind.
    pub fn from_json(file: &[u8]) -> Result<Self, String> {
        let f: FixtureFile =
            serde_json::from_slice(file).map_err(|e| format!("the fixture: {e}"))?;
        Ok(Self {
            driver: f.driver.into(),
            socket: f.socket,
            host_restart: f.host_restart.map(Launch::from),
            content: f.content,
            volume_root: f.volume_root,
            probe: f.probe,
            uid_base: f.uid_base,
            uid_count: f.uid_count,
            objects: f.objects,
        })
    }

    /// This fixture as a file holds it, in JSON.
    ///
    /// # Errors
    /// An argument is not text, which JSON cannot hold.
    pub fn to_json(&self) -> Result<Vec<u8>, String> {
        let f = FixtureFile {
            driver: (&self.driver).try_into()?,
            socket: self.socket.clone(),
            host_restart: self.host_restart.as_ref().map(LaunchFile::try_from).transpose()?,
            content: self.content.clone(),
            volume_root: self.volume_root.clone(),
            probe: self.probe.clone(),
            uid_base: self.uid_base,
            uid_count: self.uid_count,
            objects: self.objects,
        };
        serde_json::to_vec_pretty(&f).map_err(|e| e.to_string())
    }

    /// The id range of the suite's `n`th object, if the fixture has one for it.
    #[must_use]
    pub fn ids(&self, n: u32) -> Option<(u32, u32)> {
        if n >= self.objects {
            return None;
        }
        let base = self.uid_base.checked_add(n.checked_mul(self.uid_count)?)?;
        base.checked_add(self.uid_count)?;
        Some((base, self.uid_count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixture_file_reads_back_as_it_was_written() {
        let fixture = Fixture {
            driver: Launch { program: "/d".into(), args: vec!["--socket".into(), "/s".into()] },
            socket: "/s".into(),
            host_restart: Some(Launch { program: "/bin/sh".into(), args: vec!["-c".into()] }),
            content: "/c".into(),
            volume_root: "/v".into(),
            probe: "/p".into(),
            uid_base: 1 << 30,
            uid_count: 10,
            objects: 3,
        };
        let read = Fixture::from_json(&fixture.to_json().unwrap()).unwrap();
        assert_eq!(read.driver, fixture.driver);
        assert_eq!(read.host_restart, fixture.host_restart);
        assert_eq!((read.uid_base, read.uid_count, read.objects), (1 << 30, 10, 3));
    }

    #[test]
    fn a_fixture_file_may_leave_out_what_has_a_default_and_nothing_else() {
        let least = br#"{"driver": {"program": "/d"}, "socket": "/s", "content": "/c",
            "volume_root": "/v", "probe": "/p", "uid_base": 100000}"#;
        let read = Fixture::from_json(least).unwrap();
        assert_eq!((read.uid_count, read.objects, read.host_restart), (IDS, OBJECTS, None));
        assert!(Fixture::from_json(br#"{"driver": {"program": "/d"}}"#).is_err());
        let unknown = br#"{"driver": {"program": "/d"}, "socket": "/s", "content": "/c",
            "volume_root": "/v", "probe": "/p", "uid_base": 1, "sockets": "/t"}"#;
        assert!(Fixture::from_json(unknown).unwrap_err().contains("sockets"));
    }
}
