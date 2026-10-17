// SPDX-License-Identifier: AGPL-3.0-or-later

//! The official PostgreSQL image, run on this machine by podman or docker, for evaluation: §2 of
//! docs/18-operations.md says why a database in a container is not a production configuration,
//! and installation says so when it makes one. The image is pinned by digest, its data is in a
//! named volume, and it is published on loopback alone. Docker restarts it itself, with
//! `--restart unless-stopped`, since its daemon starts at boot; podman has no daemon, so a
//! systemd unit runs it, and without units (`--no-system`) nothing restarts it. Its superuser's
//! password is made, used once to make the owner, and not kept: `exec` into the container reaches
//! the superuser over the local socket, which is what anyone who may run the engine can do anyway.

use super::{OWNER_SQL, owner_url, password, run};

/// The image a container is run from: `postgres:17` for every architecture the official image
/// is built for, by its index digest. Changed by a commit that says why.
pub const IMAGE: &str = "docker.io/library/postgres:17@sha256:ae69c452f483507a6b99fb654cf93aad7fe156ffd2c56247707eef4e36d3c12b";

/// What runs the container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// podman, run by a systemd unit.
    Podman,
    /// docker, restarted by its daemon.
    Docker,
}

impl Engine {
    /// The command.
    #[must_use]
    pub fn program(self) -> &'static str {
        match self {
            Self::Podman => "podman",
            Self::Docker => "docker",
        }
    }

    /// The engine on `PATH`, podman first since it needs no daemon running as root.
    #[must_use]
    pub fn found() -> Option<Self> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        [Self::Podman, Self::Docker]
            .into_iter()
            .find(|e| std::env::split_paths(&path).any(|d| d.join(e.program()).is_file()))
    }
}

/// The container to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    /// What runs it.
    pub engine: Engine,
    /// Its name; its volume is named after it.
    pub name: String,
    /// The image, by digest.
    pub image: String,
    /// The loopback port it is published on.
    pub port: u16,
}

impl Container {
    /// The volume its data is in.
    #[must_use]
    pub fn volume(&self) -> String {
        format!("{}-data", self.name)
    }

    /// The arguments that make it: run at once and restarted by docker, or made for a unit to
    /// run by podman. The superuser's password is not among them; it is given to the engine in
    /// `POSTGRES_PASSWORD`, in its environment.
    #[must_use]
    pub fn make(&self) -> Vec<String> {
        let mut argv: Vec<String> = match self.engine {
            Engine::Docker => {
                ["docker", "run", "-d", "--restart", "unless-stopped"].map(Into::into).into()
            }
            Engine::Podman => ["podman", "create"].map(Into::into).into(),
        };
        argv.extend([
            "--name".into(),
            self.name.clone(),
            "--label".into(),
            "dev.korpis.role=postgres".into(),
            // By name alone: the engine reads the value from its own environment, so the
            // password is never in an argument any user of the machine can read.
            "-e".into(),
            "POSTGRES_PASSWORD".into(),
            "-p".into(),
            format!("127.0.0.1:{}:5432", self.port),
            "-v".into(),
            format!("{}:/var/lib/postgresql/data", self.volume()),
            self.image.clone(),
        ]);
        argv
    }

    /// The unit that runs a podman container: none for docker, whose daemon restarts it.
    #[must_use]
    pub fn unit(&self) -> Option<(String, String)> {
        (self.engine == Engine::Podman).then(|| {
            let name = &self.name;
            let unit = format!(
                "[Unit]
Description=PostgreSQL for Korpis, in the podman container {name} (for evaluation)
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/bin/podman start --attach {name}
ExecStop=/usr/bin/podman stop --time 30 {name}
Restart=on-failure
RestartSec=2

[Install]
WantedBy=multi-user.target
"
            );
            (format!("{name}.service"), unit)
        })
    }
}

/// Makes the container, unless one of its name is there: run at once by docker, created for a
/// unit or [`start`] by podman. The superuser's password, which [`finish`] needs.
///
/// # Errors
/// A container of the name is there, or the engine refused.
pub fn make(c: &Container) -> std::io::Result<String> {
    let engine = c.engine.program();
    if run(&[engine, "container", "inspect", &c.name], &[], None).is_ok() {
        return Err(std::io::Error::other(format!(
            "a container named {} is there: remove it, or name another with --postgres-container",
            c.name
        )));
    }
    let superuser = password()?;
    let make = c.make();
    let env = [("POSTGRES_PASSWORD", superuser.as_str())];
    run(&make.iter().map(String::as_str).collect::<Vec<_>>(), &env, None)?;
    Ok(superuser)
}

/// Starts a podman container that no unit runs.
///
/// # Errors
/// podman refused.
pub fn start(c: &Container) -> std::io::Result<()> {
    run(&[c.engine.program(), "start", &c.name], &[], None).map(drop)
}

/// Waits for the server the container runs, makes the owner and its database as `superuser`, and
/// returns the owner's URL.
///
/// # Errors
/// The server never answered, or refused.
pub async fn finish(c: &Container, superuser: &str) -> std::io::Result<String> {
    let ready = format!("postgres://postgres:{superuser}@127.0.0.1:{}/postgres", c.port);
    super::wait_ready(&ready, 120).await.map_err(|e| std::io::Error::other(e.to_string()))?;
    let password = password()?;
    let mut psql = vec![c.engine.program(), "exec", "-i", &c.name, "psql", "-X", "-q"];
    psql.extend(["-U", "postgres", "-v", "ON_ERROR_STOP=1"]);
    run(&psql, &[], Some(&format!("\\set pw '{password}'\n{OWNER_SQL}")))?;
    Ok(owner_url(&password, c.port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_restarts_its_container_and_podman_s_is_run_by_a_unit_on_loopback_alone() {
        let docker = Container {
            engine: Engine::Docker,
            name: "korpis-postgres".into(),
            image: IMAGE.into(),
            port: 5432,
        };
        let made = docker.make().join(" ");
        assert!(made.contains("-e POSTGRES_PASSWORD -p"), "the password is not an argument");
        assert!(made.starts_with("docker run -d --restart unless-stopped --name korpis-postgres"));
        assert!(made.contains("-p 127.0.0.1:5432:5432 -v korpis-postgres-data:/var/lib/"));
        assert!(made.ends_with(IMAGE) && IMAGE.contains("@sha256:"));
        assert_eq!(docker.unit(), None);
        let podman = Container { engine: Engine::Podman, ..docker };
        assert!(podman.make().join(" ").starts_with("podman create --name"));
        let (name, unit) = podman.unit().unwrap();
        assert_eq!(name, "korpis-postgres.service");
        assert!(unit.contains("ExecStart=/usr/bin/podman start --attach korpis-postgres"));
    }
}
