// SPDX-License-Identifier: AGPL-3.0-or-later

//! What installation writes to the machine besides the data directory (§2 of
//! docs/18-operations.md): the users the units run as, made by `systemd-sysusers` from a file of
//! its own; the units; and the environment file the control plane reads its database URL from,
//! readable by root alone, since systemd reads it before the service drops to its user.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;

/// A user's uid and primary gid, from a passwd file at `passwd`.
#[must_use]
pub fn user(passwd: &Path, name: &str) -> Option<(u32, u32)> {
    fs::read_to_string(passwd).ok()?.lines().find_map(|l| {
        let mut f = l.split(':');
        (f.next()? == name).then_some(())?;
        let _password = f.next()?;
        Some((f.next()?.parse().ok()?, f.next()?.parse().ok()?))
    })
}

/// Writes `text` to `path`, replacing what is there, with `mode`, through a file beside it.
fn replace(path: &Path, text: &str, mode: u32) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut beside = path.as_os_str().to_owned();
    beside.push(".new");
    let beside = Path::new(&beside);
    let _ = fs::remove_file(beside);
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(beside)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    fs::set_permissions(beside, fs::Permissions::from_mode(mode))?;
    fs::rename(beside, path)
}

/// Writes the sysusers file and has `systemd-sysusers` make the users it names.
///
/// # Errors
/// The file could not be written, or `systemd-sysusers` failed.
pub fn make_users(file: &Path, text: &str) -> std::io::Result<()> {
    replace(file, text, 0o644)?;
    let made = std::process::Command::new("systemd-sysusers").arg(file).status()?;
    if made.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("systemd-sysusers {}: {made}", file.display())))
    }
}

/// Writes each unit into `dir`.
///
/// # Errors
/// A unit could not be written.
pub fn write_units(dir: &Path, units: &[super::units::Unit]) -> std::io::Result<()> {
    for (name, text) in units {
        replace(&dir.join(name), text, 0o644)?;
    }
    Ok(())
}

/// The environment file's text: one `KEY=value` a line, in the order given.
#[must_use]
pub fn environment(values: &[(&str, String)]) -> String {
    let mut out = String::from("# Written by korpis-server install; read by systemd as root.\n");
    for (key, value) in values {
        out.push_str(key);
        out.push('=');
        out.push_str(value);
        out.push('\n');
    }
    out
}

/// Writes the environment file at `path`, root's alone: it holds the database password.
///
/// # Errors
/// It could not be written.
pub fn write_environment(path: &Path, values: &[(&str, String)]) -> std::io::Result<()> {
    replace(path, &environment(values), 0o600)
}

/// Gives `path` and everything beneath it to `owner`.
///
/// # Errors
/// An entry could not be read or given.
pub fn give(path: &Path, owner: (u32, u32)) -> std::io::Result<()> {
    std::os::unix::fs::lchown(path, Some(owner.0), Some(owner.1))?;
    if fs::symlink_metadata(path)?.is_dir() {
        for entry in fs::read_dir(path)? {
            give(&entry?.path(), owner)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_is_read_from_passwd_and_the_environment_is_root_s_alone() {
        let dir = tempfile::tempdir().unwrap();
        let passwd = dir.path().join("passwd");
        fs::write(&passwd, "root:x:0:0::/root:/bin/sh\nkorpis-agent:x:991:987::/var/lib:/x\n")
            .unwrap();
        assert_eq!(user(&passwd, "korpis-agent"), Some((991, 987)));
        assert_eq!(user(&passwd, "korpis"), None);
        let env = dir.path().join("etc/korpis/server.env");
        let values = [("KORPIS_DATABASE_URL", "postgres://korpis_server:pw@h/db".to_owned())];
        write_environment(&env, &values).unwrap();
        let mode = fs::metadata(&env).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(
            fs::read_to_string(&env)
                .unwrap()
                .ends_with("KORPIS_DATABASE_URL=postgres://korpis_server:pw@h/db\n")
        );
    }
}
