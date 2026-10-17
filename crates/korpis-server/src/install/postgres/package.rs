// SPDX-License-Identifier: AGPL-3.0-or-later

//! The distribution's own PostgreSQL, installed by its package manager: `apt-get` on Debian and
//! its derivatives, `dnf` on Fedora and the Red Hat family. Whatever major the distribution
//! packages is what is installed, and preflight then says whether it is the one Korpis is tested
//! against. The cluster is the distribution's, listening on loopback as it does by default; on
//! the Red Hat family, where the installer creates it, it is created with password
//! authentication over TCP, since that family's default, `ident`, admits no URL. The installer
//! makes the owner as the `postgres` system user, over the server's local socket.

use super::{OWNER_SQL, owner_url, password, run};
use std::path::Path;

/// A package manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    /// Debian, Ubuntu, and their derivatives.
    Apt,
    /// Fedora, Red Hat Enterprise Linux, `CentOS` Stream, Rocky, and Alma.
    Dnf,
}

/// The package manager `/etc/os-release`'s text names, by `ID` or `ID_LIKE`.
#[must_use]
pub fn detect(os_release: &str) -> Option<Manager> {
    let field = |key: &str| {
        os_release.lines().find_map(|l| {
            let value = l.strip_prefix(key)?.strip_prefix('=')?;
            Some(value.trim().trim_matches('"').to_owned())
        })
    };
    let ids = [field("ID"), field("ID_LIKE")].into_iter().flatten().collect::<Vec<_>>().join(" ");
    let named = |family: &[&str]| ids.split_whitespace().any(|id| family.contains(&id));
    if named(&["debian", "ubuntu"]) {
        Some(Manager::Apt)
    } else if named(&["fedora", "rhel", "centos"]) {
        Some(Manager::Dnf)
    } else {
        None
    }
}

/// One command: its environment, its arguments, and a file whose presence means it is done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// What it is run with.
    pub env: Vec<(&'static str, &'static str)>,
    /// The command.
    pub argv: Vec<&'static str>,
    /// Skipped when this exists.
    pub unless: Option<&'static str>,
}

impl std::fmt::Display for Step {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (key, value) in &self.env {
            write!(f, "{key}='{value}' ")?;
        }
        write!(f, "{}", self.argv.join(" "))?;
        if let Some(path) = self.unless {
            write!(f, "    (unless {path} exists)")?;
        }
        Ok(())
    }
}

fn step(argv: &[&'static str]) -> Step {
    Step { env: Vec::new(), argv: argv.to_vec(), unless: None }
}

/// What installs and starts the server, in order.
#[must_use]
pub fn steps(m: Manager) -> Vec<Step> {
    match m {
        Manager::Apt => vec![
            step(&["apt-get", "update"]),
            Step {
                env: vec![("DEBIAN_FRONTEND", "noninteractive")],
                ..step(&["apt-get", "install", "-y", "postgresql"])
            },
            step(&["systemctl", "enable", "--now", "postgresql.service"]),
        ],
        Manager::Dnf => vec![
            step(&["dnf", "install", "-y", "postgresql-server"]),
            Step {
                env: vec![(
                    "PGSETUP_INITDB_OPTIONS",
                    "--auth-host=scram-sha-256 --auth-local=peer",
                )],
                unless: Some("/var/lib/pgsql/data/PG_VERSION"),
                ..step(&["postgresql-setup", "--initdb"])
            },
            step(&["systemctl", "enable", "--now", "postgresql.service"]),
        ],
    }
}

/// `psql` as the server's superuser, the `postgres` system user, over the local socket.
const AS_POSTGRES: [&str; 8] = ["runuser", "-u", "postgres", "--", "psql", "-X", "-q", "-d"];

/// Installs and starts the server, makes the owner and its database, and returns the owner's
/// URL.
///
/// # Errors
/// A step failed, or the server did not say its port.
pub fn provision(m: Manager) -> std::io::Result<String> {
    for s in steps(m) {
        if s.unless.is_some_and(|p| Path::new(p).exists()) {
            continue;
        }
        run(&s.argv, &s.env, None)?;
    }
    let password = password()?;
    let mut psql = AS_POSTGRES.to_vec();
    psql.extend(["postgres", "-v", "ON_ERROR_STOP=1"]);
    run(&psql, &[], Some(&format!("\\set pw '{password}'\n{OWNER_SQL}")))?;
    let mut show = AS_POSTGRES.to_vec();
    show.extend(["postgres", "-tA", "-c", "SHOW port"]);
    let port = run(&show, &[], None)?;
    let port = port.trim().parse().map_err(|_| std::io::Error::other("the server's port"))?;
    Ok(owner_url(&password, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manager_is_the_distribution_s_and_red_hat_s_cluster_takes_passwords() {
        let debian = "PRETTY_NAME=\"Debian GNU/Linux 12\"\nID=debian\n";
        let ubuntu = "NAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=debian\n";
        let rocky = "ID=\"rocky\"\nID_LIKE=\"rhel centos fedora\"\n";
        assert_eq!(detect(debian), Some(Manager::Apt));
        assert_eq!(detect(ubuntu), Some(Manager::Apt));
        assert_eq!(detect(rocky), Some(Manager::Dnf));
        assert_eq!(detect("ID=alpine\n"), None);
        let initdb = steps(Manager::Dnf).into_iter().find(|s| s.argv[0] == "postgresql-setup");
        let initdb = initdb.unwrap().to_string();
        assert!(initdb.contains("--auth-host=scram-sha-256"), "{initdb}");
        assert!(initdb.ends_with("(unless /var/lib/pgsql/data/PG_VERSION exists)"));
    }
}
