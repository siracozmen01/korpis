// SPDX-License-Identifier: AGPL-3.0-or-later

//! An iSCSI class's device: a LUN of a target, logged in with open-iscsi's `iscsiadm`, which
//! keeps a node record of the target with the CHAP credential the provider answered, logs in,
//! and lets udev name the LUN's disk by its path. Letting go of it logs out and deletes the
//! record, so no credential outlives its attachment on the node. Each part is checked before it
//! reaches the command line, and the credential before it reaches the record, which `iscsiadm`
//! writes line by line. What it costs: the credential is an argument of `iscsiadm` while it
//! runs, readable from `/proc` by the node's own users, never by a workload, which sees none of
//! the host's processes.

use crate::error::{HelperError, HelperResult};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

/// How long udev is given to name the disk once logged in.
const NAMED_WITHIN: Duration = Duration::from_secs(15);
/// `iscsiadm`'s exit codes for a session that exists already and for none to log out of.
const SESSION_EXISTS: i32 = 15;
const NO_SESSION: i32 = 21;

/// An iSCSI target as a provider answers it: `<portal>|<iqn>|<lun>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Target {
    portal: String,
    iqn: String,
    lun: u32,
}

impl Target {
    /// The target `s` says, each part checked.
    pub(super) fn parse(s: &str) -> HelperResult<Self> {
        let mut parts = s.split('|');
        let (Some(portal), Some(iqn), Some(lun), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(HelperError::Refused(format!("iSCSI target {s:?}: <portal>|<iqn>|<lun>")));
        };
        super::plain("iSCSI portal", portal, ":[]")?;
        super::plain("iSCSI qualified name", iqn, ":")?;
        let lun = lun.parse().map_err(|_| HelperError::Refused(format!("iSCSI LUN {lun:?}")))?;
        Ok(Self { portal: portal.into(), iqn: iqn.into(), lun })
    }

    /// The disk udev names the LUN by once logged in.
    fn disk(&self) -> PathBuf {
        let portal = self.portal.trim_start_matches('[').replace(']', "");
        PathBuf::from(format!("/dev/disk/by-path/ip-{portal}-iscsi-{}-lun-{}", self.iqn, self.lun))
    }

    /// `iscsiadm`'s arguments naming the target's node record, then `rest`.
    fn node(&self, rest: &[&str]) -> Vec<String> {
        let mut args: Vec<String> = ["-m", "node", "-T", &self.iqn, "-p", &self.portal]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        args.extend(rest.iter().map(|s| (*s).to_owned()));
        args
    }
}

/// The CHAP user and secret `credential` says, `<user>:<secret>`; none for an empty one.
fn chap(credential: &str) -> HelperResult<Option<(&str, &str)>> {
    if credential.is_empty() {
        return Ok(None);
    }
    let refused = || HelperError::Refused("an iSCSI credential is <user>:<secret>".into());
    let (user, secret) = credential.split_once(':').ok_or_else(refused)?;
    super::plain("CHAP user", user, "@:")?;
    let printable = secret.chars().all(|c| c.is_ascii_graphic());
    if secret.is_empty() || secret.len() > 255 || !printable {
        return Err(refused());
    }
    Ok(Some((user, secret)))
}

/// Runs `iscsiadm` with `args`; its exit code, an error naming what it said if it failed with
/// any code but those in `fine`.
fn iscsiadm(args: &[String], fine: &[i32]) -> HelperResult<i32> {
    let ran = Command::new("iscsiadm").args(args).stdin(std::process::Stdio::null()).output()?;
    let code = ran.status.code().unwrap_or(-1);
    if ran.status.success() || fine.contains(&code) {
        return Ok(code);
    }
    let said = String::from_utf8_lossy(&ran.stderr).into_owned();
    Err(HelperError::Failed(format!("iscsiadm {}: {said}", args.get(..2).unwrap_or(&[]).join(" "))))
}

/// The commands that keep `t`'s node record with `credential` and log in to it.
fn login(t: &Target, credential: &str) -> HelperResult<Vec<Vec<String>>> {
    let mut commands = vec![t.node(&["-o", "new"])];
    if let Some((user, secret)) = chap(credential)? {
        for (name, value) in [
            ("node.session.auth.authmethod", "CHAP"),
            ("node.session.auth.username", user),
            ("node.session.auth.password", secret),
        ] {
            commands.push(t.node(&["-o", "update", "-n", name, "-v", value]));
        }
    }
    commands.push(t.node(&["--login"]));
    Ok(commands)
}

/// Logs in to `t` presenting `credential`, or finds it logged in; the LUN's disk.
pub(super) fn attach(t: &Target, credential: &str) -> HelperResult<PathBuf> {
    for args in login(t, credential)? {
        iscsiadm(&args, &[SESSION_EXISTS])?;
    }
    let disk = t.disk();
    let since = Instant::now();
    while !disk.exists() {
        if since.elapsed() > NAMED_WITHIN {
            return Err(HelperError::Failed(format!("{} never appeared", disk.display())));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(disk)
}

/// Logs out of `t` and deletes its node record, with the credential in it.
pub(super) fn release(t: &Target) -> HelperResult<()> {
    iscsiadm(&t.node(&["--logout"]), &[NO_SESSION])?;
    iscsiadm(&t.node(&["-o", "delete"]), &[NO_SESSION])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_is_a_portal_a_name_and_a_lun_and_nothing_reaches_a_command_unchecked() {
        let t = Target::parse("10.0.0.5:3260|iqn.2026-10.dev.korpis:fast|3").unwrap();
        let disk = "/dev/disk/by-path/ip-10.0.0.5:3260-iscsi-iqn.2026-10.dev.korpis:fast-lun-3";
        assert_eq!(t.disk(), PathBuf::from(disk));
        for bad in ["a|b", "a b|iqn|1", "p|iqn;rm|1", "p|iqn|x", "p|iqn|1|2", "p|iqn\n|1"] {
            assert!(Target::parse(bad).is_err(), "{bad:?}");
        }
        let commands = login(&t, "korpis:s3cret!").unwrap();
        assert_eq!(commands.len(), 5, "a record, three settings, and the login");
        let last = commands.last().unwrap().join(" ");
        assert_eq!(last, "-m node -T iqn.2026-10.dev.korpis:fast -p 10.0.0.5:3260 --login");
        assert_eq!(login(&t, "").unwrap().len(), 2, "no credential, no settings");
        for bad in ["nouser", "u:sec ret", "u:line\nbreak", "u v:s"] {
            assert!(chap(bad).is_err(), "{bad:?}");
        }
    }
}
