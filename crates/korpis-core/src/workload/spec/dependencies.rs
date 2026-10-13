// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dependencies declared explicitly (§5 of docs/07-networking.md): what a workload reaches by
//! name, so the graph is visible to the scheduler and to plans, and its address is injected at
//! start rather than discovered.

use super::SpecError;
use crate::labels::LabelSelector;
use uuid::Uuid;

/// What a dependency reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyTarget {
    /// One workload, by identifier.
    Workload(Uuid),
    /// One endpoint, by identifier.
    Endpoint(Uuid),
    /// The workloads a selector matches within the dependent's project.
    Selector(LabelSelector),
}

/// How the address is handed to the workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Injection {
    /// `KORPIS_DEP_<NAME>_HOST` and `_PORT`.
    Env,
    /// `/korpis/deps/<name>.json`, for workloads that reload.
    File,
    /// Both.
    #[default]
    Both,
}

/// One dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    /// What the injected names are built from.
    pub name: String,
    /// What it reaches.
    pub target: DependencyTarget,
    /// How the address arrives.
    pub inject: Injection,
    /// Start only once the dependency is healthy.
    pub required: bool,
}

/// A dependency's name: `[a-z][a-z0-9_]*`, at most 32 bytes, so `KORPIS_DEP_<NAME>_HOST` is a
/// valid environment variable once uppercased.
fn check(field: &str, name: &str) -> Result<(), SpecError> {
    let b = name.as_bytes();
    let ok = !b.is_empty()
        && b.len() <= 32
        && b.first().is_some_and(u8::is_ascii_lowercase)
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_');
    if ok { Ok(()) } else { Err(SpecError::new(field, "[a-z][a-z0-9_]*, at most 32")) }
}

/// Checks every dependency's name, and that no two share one.
pub fn validate(dependencies: &[Dependency]) -> Result<(), SpecError> {
    for (i, d) in dependencies.iter().enumerate() {
        let field = format!("dependencies[{i}].name");
        check(&field, &d.name)?;
        if dependencies.iter().take(i).any(|e| e.name == d.name) {
            return Err(SpecError::new(&field, "declared twice"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> Dependency {
        Dependency {
            name: name.into(),
            target: DependencyTarget::Workload(Uuid::nil()),
            inject: Injection::Both,
            required: false,
        }
    }

    #[test]
    fn a_name_becomes_an_environment_variable_and_is_declared_once() {
        assert!(validate(&[named("db"), named("cache_2")]).is_ok());
        assert!(validate(&[named("db"), named("db")]).is_err());
        for bad in ["", "Db", "2db", "my-db", &"x".repeat(33)] {
            assert!(validate(&[named(bad)]).is_err(), "{bad:?}");
        }
    }
}
