// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control plane's preflight (§2 of docs/18-operations.md), run by `korpis-server install`
//! before it changes anything: the PostgreSQL server, what the installing role may do on it,
//! whether it is shared, the data directory, and the backup target declared outside Korpis. It
//! reads; the one thing it writes is a probe file in the backup target, made and removed.

use super::target::{self, BackupTarget};
use korpis_core::preflight::{Check, Outcome, Report};
use korpis_store::server::ServerFacts;
use std::path::Path;

/// The oldest PostgreSQL whose features the schema uses: 13, for `pg_current_snapshot`.
const OLDEST: i32 = 130_000;
/// The PostgreSQL major the build is tested against.
const TESTED: i32 = 17;

/// What the installation needs of the machine and the server.
#[derive(Debug, Clone)]
pub struct Asked<'a> {
    /// What the server said.
    pub facts: &'a ServerFacts,
    /// Whether the control plane is given a login of the operator's own rather than one made.
    pub own_login: bool,
    /// The data directory.
    pub data: &'a Path,
    /// The backup target declared.
    pub target: Option<&'a BackupTarget>,
}

/// Every check, in the order they are worth reading.
#[must_use]
pub fn control_plane(a: &Asked<'_>) -> Report {
    Report(vec![
        version(a.facts),
        installed(a.facts),
        objects(a.facts),
        roles(a.facts, a.own_login),
        shared(a.facts),
        data_directory(a.data),
        target::check(a.target, a.data),
    ])
}

fn check(name: &'static str, needed_for: &'static str, found: String, outcome: Outcome) -> Check {
    let essential = !matches!(outcome, Outcome::Unchecked(_));
    Check { name, needed_for, found, outcome, essential }
}

fn version(f: &ServerFacts) -> Check {
    let major = f.version_num / 10_000;
    let found = format!("PostgreSQL {}", f.version);
    let outcome = if f.version_num < OLDEST {
        Outcome::Lacking {
            costs: "everything: the schema uses what 13 added".into(),
            fix: format!("PostgreSQL {TESTED}, which Korpis is tested against"),
        }
    } else if major == TESTED {
        Outcome::Met
    } else {
        Outcome::Unchecked(format!(
            "Korpis is tested against PostgreSQL {TESTED}; {major} has every feature it uses, \
             and is not tested"
        ))
    };
    let mut c = check("PostgreSQL 13+", "everything", found, outcome);
    c.essential = f.version_num < OLDEST;
    c
}

fn installed(f: &ServerFacts) -> Check {
    let (found, outcome) = if f.installed {
        (
            "holds an installation".to_owned(),
            Outcome::Lacking {
                costs: "nothing is installed over an installation".into(),
                fix: "name an empty database; an installation is upgraded, not installed again \
                      (§4 of docs/18-operations.md)"
                    .into(),
            },
        )
    } else {
        (format!("no installation, {} of {} migrations", f.applied, f.bundled), Outcome::Met)
    };
    check("an empty database", "installing", found, outcome)
}

fn objects(f: &ServerFacts) -> Check {
    let outcome = if f.creates_objects {
        Outcome::Met
    } else {
        Outcome::Lacking {
            costs: "the schema cannot be made".into(),
            fix: "connect as the database's owner, or GRANT CREATE ON DATABASE to the role".into(),
        }
    };
    let found = if f.creates_objects { "may create" } else { "may not create" }.to_owned();
    check("schema rights", "migrations", found, outcome)
}

fn roles(f: &ServerFacts, own_login: bool) -> Check {
    let outcome = if own_login {
        Outcome::NotNeeded("--database-url names the control plane's login".into())
    } else if f.creates_roles {
        Outcome::Met
    } else {
        Outcome::Lacking {
            costs: "the control plane's own login, a member of korpis_app, cannot be made".into(),
            fix: "connect as a role with CREATEROLE, or make a login that is a member of \
                  korpis_app and name it with --database-url"
                .into(),
        }
    };
    let found = if f.creates_roles { "may create roles" } else { "may not create roles" };
    check("role rights", "the control plane's login", found.to_owned(), outcome)
}

fn shared(f: &ServerFacts) -> Check {
    let found = format!("{} other databases", f.other_databases);
    let outcome = if f.other_databases == 0 {
        Outcome::Met
    } else {
        Outcome::Lacking {
            costs: "a transaction another application leaves open holds back every event \
                    subscriber until it ends (§2 of docs/18-operations.md)"
                .into(),
            fix: "a PostgreSQL server of Korpis's own, or, for each other application's role, \
                  ALTER ROLE <role> SET idle_in_transaction_session_timeout = '60s'"
                .into(),
        }
    };
    let mut c = check("a server of its own", "timely events", found, outcome);
    c.essential = false;
    c
}

fn data_directory(data: &Path) -> Check {
    let (name, needed_for) = ("data directory", "the signing keys and TLS material");
    let tmpfs = on_tmpfs(data);
    let found = format!("{}", data.display());
    let outcome = if !data.is_absolute() {
        Outcome::Lacking { costs: "nothing can be kept".into(), fix: "an absolute path".into() }
    } else if tmpfs {
        Outcome::Lacking {
            costs: "the keys are lost at the next reboot".into(),
            fix: "a directory on a disk, /var/lib/korpis-server by default".into(),
        }
    } else if opens_to_others(data) {
        Outcome::Lacking {
            costs: "every user of the machine can list the keys and what is beside them".into(),
            fix: format!("chmod 0700 {}", data.display()),
        }
    } else {
        Outcome::Met
    };
    check(name, needed_for, found, outcome)
}

/// Whether `path` is there and users other than its owner may open it.
fn opens_to_others(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o077 != 0)
}

/// Whether `path` is on a tmpfs, whose contents a reboot loses: the filesystem of the longest
/// mount point in `/proc/self/mounts` that holds it.
fn on_tmpfs(path: &Path) -> bool {
    let Ok(mounts) = std::fs::read_to_string("/proc/self/mounts") else { return false };
    mounts
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (_, point, kind) = (f.next()?, f.next()?, f.next()?);
            path.starts_with(point).then_some((point.len(), kind == "tmpfs"))
        })
        .max_by_key(|(len, _)| *len)
        .is_some_and(|(_, tmpfs)| tmpfs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> ServerFacts {
        ServerFacts {
            version_num: 170_004,
            version: "17.4".into(),
            creates_roles: true,
            creates_objects: true,
            applied: 0,
            bundled: 80,
            other_databases: 0,
            installed: false,
        }
    }

    fn lacking(r: &Report) -> Vec<&'static str> {
        let lacks = |c: &&Check| matches!(c.outcome, Outcome::Lacking { .. });
        r.0.iter().filter(lacks).map(|c| c.name).collect()
    }

    #[test]
    fn an_installation_is_refused_over_another_on_an_old_server_or_without_a_backup_target() {
        let dir = tempfile::tempdir().unwrap();
        let (data, elsewhere) = (dir.path().join("data"), Path::new("/proc"));
        let target = BackupTarget::Directory(elsewhere.into());
        let ask = |facts, target| Asked { facts, own_login: false, data: &data, target };
        let good = facts();
        let none = control_plane(&ask(&good, None));
        assert_eq!(lacking(&none), ["control-plane backup target"]);
        assert!(!none.offers_anything());
        let old = ServerFacts { version_num: 120_000, ..facts() };
        let installed = ServerFacts { installed: true, ..facts() };
        assert!(lacking(&control_plane(&ask(&old, Some(&target)))).contains(&"PostgreSQL 13+"));
        let over = control_plane(&ask(&installed, Some(&target)));
        assert!(lacking(&over).contains(&"an empty database"));
        let newer = ServerFacts { version_num: 180_001, ..facts() };
        let newer = control_plane(&ask(&newer, Some(&target)));
        assert!(newer.0.iter().any(|c| matches!(c.outcome, Outcome::Unchecked(_))));
    }

    #[test]
    fn a_shared_server_is_a_warning_and_role_rights_are_not_needed_with_a_login_of_one_s_own() {
        let shared = ServerFacts { other_databases: 3, creates_roles: false, ..facts() };
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let own = Asked { facts: &shared, own_login: true, data: &data, target: None };
        let report = control_plane(&own);
        let check = |name| report.0.iter().find(|c| c.name == name).unwrap();
        assert!(!check("a server of its own").essential);
        assert!(matches!(check("role rights").outcome, Outcome::NotNeeded(_)));
        let made = Asked { own_login: false, ..own };
        assert!(lacking(&control_plane(&made)).contains(&"role rights"));
    }

    #[test]
    fn a_data_directory_others_can_open_is_lacking() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let open = data_directory(dir.path());
        assert!(matches!(open.outcome, Outcome::Lacking { .. }), "{open:?}");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!matches!(data_directory(dir.path()).outcome, Outcome::Lacking { .. }));
    }
}
