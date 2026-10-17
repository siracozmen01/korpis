// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-server migrate` (§4 of docs/18-operations.md): applies what this build carries, or,
//! with `--to`, undoes what a newer build applied, so that an older binary can be put back. It
//! runs as the schema's owner, named by flag or environment, or read from the file installation
//! keeps it in.

use clap::Args;
use korpis_store::schema;
use std::fmt::Write as _;
use std::path::PathBuf;

/// What `migrate` is told.
#[derive(Debug, Args)]
pub struct MigrateArgs {
    /// A database URL with schema-owner rights; unset, read from --owner-env.
    #[arg(long, env = "KORPIS_DATABASE_OWNER_URL")]
    database_owner_url: Option<String>,
    /// The file installation keeps the owner's URL in.
    #[arg(long, default_value = "/etc/korpis/owner.env")]
    owner_env: PathBuf,
    /// The version to move to; by default the newest this build carries. An earlier one undoes
    /// the migrations after it, newest first, each by its inverse.
    #[arg(long)]
    to: Option<i64>,
    /// Say what would be applied or undone, and change nothing.
    #[arg(long)]
    dry_run: bool,
}

type Failure = Box<dyn std::error::Error>;

/// Moves the schema as `args` say.
pub async fn run(args: MigrateArgs) -> Result<(), Failure> {
    let owner = match args.database_owner_url {
        Some(url) => url,
        None => from_file(&args.owner_env)?,
    };
    let applied = schema::applied(&owner).await?;
    let planned = schema::plan(&applied, args.to)?;
    let at = applied.last().copied().unwrap_or(0);
    let change = if args.dry_run { planned } else { schema::migrate_to(&owner, args.to).await? };
    crate::say(&report(at, &change, args.dry_run));
    Ok(())
}

/// The owner's URL from an environment file's `KORPIS_DATABASE_OWNER_URL=` line.
fn from_file(path: &std::path::Path) -> Result<String, Failure> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        format!("{}: {e}; name the owner with --database-owner-url", path.display())
    })?;
    text.lines()
        .find_map(|l| l.strip_prefix("KORPIS_DATABASE_OWNER_URL="))
        .map(str::to_owned)
        .ok_or_else(|| format!("{} names no KORPIS_DATABASE_OWNER_URL", path.display()).into())
}

/// What was done, or would be.
fn report(at: i64, change: &schema::Change, dry_run: bool) -> String {
    let mut out = String::new();
    if change.apply.is_empty() && change.undo.is_empty() {
        let _ = writeln!(out, "The schema is at {at}; nothing to do.");
        return out;
    }
    let (applied, undone) =
        if dry_run { ("Would apply", "Would undo") } else { ("Applied", "Undid") };
    for (v, what) in &change.apply {
        let _ = writeln!(out, "{applied} {v:04} {what}");
    }
    for (v, what) in &change.undo {
        let _ = writeln!(out, "{undone} {v:04} {what}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_owner_is_read_from_installation_s_file_and_a_change_is_listed_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("owner.env");
        std::fs::write(&file, "# kept\nKORPIS_DATABASE_OWNER_URL=postgres://o:p@h/korpis\n")
            .unwrap();
        assert_eq!(from_file(&file).unwrap(), "postgres://o:p@h/korpis");
        assert!(from_file(&dir.path().join("absent")).is_err());
        let change = schema::Change {
            apply: Vec::new(),
            undo: vec![(76, "app reads schema version".into()), (75, "owner acts as app".into())],
        };
        assert_eq!(
            report(76, &change, true),
            "Would undo 0076 app reads schema version\nWould undo 0075 owner acts as app\n"
        );
        assert_eq!(
            report(76, &schema::Change::default(), false),
            "The schema is at 76; nothing to do.\n"
        );
    }
}
