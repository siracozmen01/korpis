// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where the PostgreSQL server comes from (§2 of docs/18-operations.md): one the operator already
//! runs, named by URL; or one the installer provisions through the system package manager or a
//! container, and then leaves alone. Either provisioned server is given the same owner, a login
//! `korpis_owner` with `CREATEROLE` and no superuser rights, owning a database `korpis`, made by
//! [`OWNER_SQL`] as the server's own superuser; what installation does next it does as that
//! owner, exactly as it would on a server the operator named.

pub mod container;
pub mod package;

use std::fmt::Write as _;
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};

/// Where the server comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A server the operator runs, named by `--database-owner-url`.
    Existing,
    /// The distribution's packaged server, installed by its package manager.
    Package,
    /// The official image, run by podman or docker on this machine.
    Container,
}

impl std::str::FromStr for Source {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "existing" => Ok(Self::Existing),
            "package" => Ok(Self::Package),
            "container" => Ok(Self::Container),
            _ => Err("one of existing, package, or container".into()),
        }
    }
}

/// The owner a provisioned server is given, and its database.
pub const OWNER: &str = "korpis_owner";
/// The database a provisioned server holds the installation in.
pub const DATABASE: &str = "korpis";

/// What makes [`OWNER`] and [`DATABASE`], read by `psql` from its standard input after
/// `\set pw '<password>'`: the role is made when it is not there and given the password either
/// way, so a second run converges rather than failing. `format` quotes the password, so it is
/// never spliced into SQL.
pub const OWNER_SQL: &str = "\
SELECT format('CREATE ROLE %I', 'korpis_owner')
 WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'korpis_owner') \\gexec
SELECT format('ALTER ROLE %I LOGIN CREATEROLE NOSUPERUSER PASSWORD %L', 'korpis_owner', :'pw')
\\gexec
SELECT format('CREATE DATABASE %I OWNER %I', 'korpis', 'korpis_owner')
 WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'korpis') \\gexec
";

/// A new password: 24 random bytes in hexadecimal, so it needs no quoting anywhere.
///
/// # Errors
/// The system's randomness was unavailable.
pub fn password() -> std::io::Result<String> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    }))
}

/// The owner's URL on the server listening on loopback at `port`.
#[must_use]
pub fn owner_url(password: &str, port: u16) -> String {
    format!("postgres://{OWNER}:{password}@127.0.0.1:{port}/{DATABASE}")
}

/// Asks the operator where the server comes from, on `out`, reading the answer from `input`;
/// asked again until the answer is one of the three.
///
/// # Errors
/// The terminal could not be read or written, or closed before an answer.
pub fn ask(input: &mut impl BufRead, out: &mut impl Write) -> std::io::Result<Source> {
    writeln!(
        out,
        "Where should PostgreSQL come from?\n\n  \
         1  a server you run already (you name it with --database-owner-url)\n  \
         2  this distribution's package, installed with its package manager\n  \
         3  the official image, run in a container (for evaluation, not production)\n"
    )?;
    loop {
        write!(out, "Choose 1, 2, or 3: ")?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Err(std::io::Error::other("no answer: the terminal closed"));
        }
        match line.trim() {
            "1" | "existing" => return Ok(Source::Existing),
            "2" | "package" => return Ok(Source::Package),
            "3" | "container" => return Ok(Source::Container),
            _ => {}
        }
    }
}

/// Runs `argv` with `env` set, feeding it `stdin`; its standard output, or why it failed.
///
/// # Errors
/// It could not be started, or exited unsuccessfully; the error carries what it wrote.
pub fn run(argv: &[&str], env: &[(&str, &str)], stdin: Option<&str>) -> std::io::Result<String> {
    let (program, rest) = argv.split_first().ok_or_else(|| std::io::Error::other("no command"))?;
    let mut command = Command::new(program);
    command.args(rest).envs(env.iter().copied()).stdout(Stdio::piped()).stderr(Stdio::piped());
    command.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child =
        command.spawn().map_err(|e| std::io::Error::other(format!("{}: {e}", argv.join(" "))))?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(text.as_bytes())?;
    }
    let output = child.wait_with_output()?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    Err(std::io::Error::other(format!("{}: {}: {}", argv.join(" "), output.status, said.trim())))
}

/// Waits until the server at `url` answers a query, for up to `seconds`.
///
/// # Errors
/// It never did; the error is the last refusal.
pub async fn wait_ready(url: &str, seconds: u64) -> Result<(), korpis_store::Error> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    loop {
        match korpis_store::server::server_facts(url).await {
            Ok(_) => return Ok(()),
            Err(e) if std::time::Instant::now() >= deadline => return Err(e),
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_question_is_asked_again_until_it_is_answered() {
        let mut out = Vec::new();
        let answer = ask(&mut "4\n\nthree\n2\n".as_bytes(), &mut out).unwrap();
        assert_eq!(answer, Source::Package);
        let asked = String::from_utf8(out).unwrap();
        assert_eq!(asked.matches("Choose 1, 2, or 3").count(), 4);
        assert!(ask(&mut "".as_bytes(), &mut Vec::new()).is_err(), "a closed terminal");
        assert!(password().unwrap().chars().all(|c| c.is_ascii_hexdigit()));
    }
}
