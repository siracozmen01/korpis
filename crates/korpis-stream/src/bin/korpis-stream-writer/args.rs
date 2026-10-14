// SPDX-License-Identifier: AGPL-3.0-or-later

//! The writer's arguments. The shim passes the query of the logging URI as `key value` pairs in
//! no particular order, so every argument is a named flag with a value.

use korpis_stream::writer::{Limits, Retention};
use std::path::PathBuf;
use std::time::Duration;

/// What the writer was told.
#[derive(Debug)]
pub struct Args {
    /// The stream's directory.
    pub dir: PathBuf,
    /// Its limits.
    pub limits: Limits,
    /// The secret values to redact, as a JSON file, if there are any.
    pub secrets: Option<PathBuf>,
    /// For an interactive workload: the FIFO the driver sends input on, and the task's stdin.
    pub input: Option<(PathBuf, PathBuf)>,
}

fn number(key: &str, value: &str) -> Result<u64, String> {
    value.parse().map_err(|_| format!("{key} {value:?} is not a number"))
}

/// The arguments `args` names.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut args = args.into_iter();
    let mut parsed =
        Args { dir: PathBuf::new(), limits: Limits::default(), secrets: None, input: None };
    let (mut input, mut stdin) = (None, None);
    while let Some(key) = args.next() {
        let value = args.next().ok_or_else(|| format!("{key} needs a value"))?;
        match key.as_str() {
            "--dir" => parsed.dir = PathBuf::from(value),
            "--secrets" => parsed.secrets = Some(PathBuf::from(value)),
            "--input" => input = Some(PathBuf::from(value)),
            "--stdin" => stdin = Some(PathBuf::from(value)),
            "--rate" => parsed.limits.rate = number(&key, &value)?,
            "--burst" => parsed.limits.burst = number(&key, &value)?,
            "--retention-bytes" => {
                parsed.limits.retention = Retention::Bytes(number(&key, &value)?);
            }
            "--retention-seconds" => {
                parsed.limits.retention =
                    Retention::Age(Duration::from_secs(number(&key, &value)?));
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if parsed.dir.as_os_str().is_empty() {
        return Err("--dir is required".to_owned());
    }
    parsed.input = match (input, stdin) {
        (Some(i), Some(s)) => Some((i, s)),
        (None, None) => None,
        _ => return Err("--input and --stdin go together".to_owned()),
    };
    Ok(parsed)
}
