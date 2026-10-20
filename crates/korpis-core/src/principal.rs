// SPDX-License-Identifier: AGPL-3.0-or-later

//! Who acts: the principals of §3.2 of docs/01-model.md, and the subject key grants are held by.
//!
//! A [`Principal`] is the structured form; a [`Subject`] is what authorization compares. Every
//! principal has exactly one key, and every key names exactly one principal, so the store can
//! index grants by key and an effect can record its actor as one string that is still
//! unambiguous when it is read back years later.

use std::fmt;
use uuid::Uuid;

/// Anything that can act, as authorization sees it: a stable key, and whether it is a set whose
/// members act through it (a chat role, an OIDC group; §5 of docs/08-identity.md).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Subject {
    /// The canonical key, as [`Principal::key`] produces it.
    pub key: String,
    /// Whether the subject is a set.
    pub set: bool,
}

impl Subject {
    /// An individual subject with `key`.
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into(), set: false }
    }

    /// A set subject with `key`.
    #[must_use]
    pub fn set(key: impl Into<String>) -> Self {
        Self { key: key.into(), set: true }
    }

    /// Whether it is an external identity, which an identity provider vouches for: every set,
    /// and every person whose key is an `ext:` key.
    #[must_use]
    pub fn is_external(&self) -> bool {
        self.set || self.key.starts_with("ext:")
    }
}

/// The control plane acting on its own decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemActor {
    /// Reconciliation of intent with observation.
    Reconciler,
    /// Placement.
    Scheduler,
    /// Installation, before any grant exists.
    Bootstrap,
    /// The installation's authority: renewing the certificate of a relay it admitted, and
    /// re-verifying the identity bindings its providers vouch for.
    Authority,
}

impl SystemActor {
    const ALL: [Self; 4] = [Self::Reconciler, Self::Scheduler, Self::Bootstrap, Self::Authority];

    /// The name used in keys.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Reconciler => "reconciler",
            Self::Scheduler => "scheduler",
            Self::Bootstrap => "bootstrap",
            Self::Authority => "authority",
        }
    }
}

/// The key of [`Principal::Anonymous`].
const ANONYMOUS: &str = "anonymous";

/// A principal, in structured form.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Principal {
    /// A person with a Korpis account.
    User(Uuid),
    /// An API token.
    Token(Uuid),
    /// Whoever holds a share link: the bearer of the link-form grant it names, which is the
    /// grant's own identifier (§6.1 of docs/08-identity.md).
    Link(Uuid),
    /// A workload acting with its own identity.
    Workload(Uuid),
    /// An installed extension, by its installation (§3.2 of docs/16-extensions.md).
    Extension(Uuid),
    /// The control plane itself.
    System(SystemActor),
    /// Whoever presented a credential that opened nothing: the actor of a failed attempt to
    /// authenticate, known only by where it came from (§10 of docs/10-api.md). It holds nothing.
    Anonymous,
    /// One person, verified by an identity provider extension.
    ExternalUser {
        /// The provider, e.g. `discord`.
        provider: String,
        /// The person's identifier at the provider.
        user: String,
    },
    /// A set at an identity provider: a role in a guild, a group claim.
    ExternalSet {
        /// The provider, e.g. `discord`.
        provider: String,
        /// The realm the set lives in, e.g. a guild or an issuer.
        realm: String,
        /// The set within the realm, e.g. a role.
        member_of: String,
    },
}

/// One set at an identity provider: a role in a guild, a group claim's value (§5 of
/// docs/08-identity.md).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SetName {
    /// Where it lives at its provider, never containing `:`: a guild, the claim naming groups.
    pub realm: String,
    /// The set within it.
    pub member_of: String,
}

impl SetName {
    /// The set as a subject of the provider whose key is `provider`.
    #[must_use]
    pub fn at(&self, provider: &str) -> Principal {
        Principal::ExternalSet {
            provider: provider.to_owned(),
            realm: self.realm.clone(),
            member_of: self.member_of.clone(),
        }
    }
}

/// Why a principal or key is malformed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrincipalError {
    /// A name part is empty, or contains the `:` that separates key parts.
    #[error("{0} must be non-empty and must not contain ':'")]
    BadPart(&'static str),
    /// The key does not name a principal.
    #[error("{0:?} is not a principal key")]
    BadKey(String),
}

fn part(name: &'static str, value: &str) -> Result<(), PrincipalError> {
    if value.is_empty() || value.contains(':') {
        return Err(PrincipalError::BadPart(name));
    }
    Ok(())
}

impl Principal {
    /// Checks the parts that keys are built from. The final part of a key may contain `:`;
    /// every earlier part may not, which is what makes a key parse back unambiguously.
    pub fn validate(&self) -> Result<(), PrincipalError> {
        match self {
            Self::ExternalUser { provider, user } => {
                part("provider", provider)?;
                if user.is_empty() { Err(PrincipalError::BadPart("user")) } else { Ok(()) }
            }
            Self::ExternalSet { provider, realm, member_of } => {
                part("provider", provider)?;
                part("realm", realm)?;
                if member_of.is_empty() {
                    Err(PrincipalError::BadPart("member_of"))
                } else {
                    Ok(())
                }
            }
            Self::User(_)
            | Self::Token(_)
            | Self::Link(_)
            | Self::Workload(_)
            | Self::Extension(_)
            | Self::System(_)
            | Self::Anonymous => Ok(()),
        }
    }

    /// The canonical key.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::User(id) => format!("user:{id}"),
            Self::Token(id) => format!("token:{id}"),
            Self::Link(grant) => format!("link:{grant}"),
            Self::Workload(id) => format!("workload:{id}"),
            Self::Extension(id) => format!("extension:{id}"),
            Self::System(actor) => format!("system:{}", actor.name()),
            Self::Anonymous => ANONYMOUS.to_owned(),
            Self::ExternalUser { provider, user } => format!("ext:{provider}:user:{user}"),
            Self::ExternalSet { provider, realm, member_of } => {
                format!("ext:{provider}:set:{realm}:{member_of}")
            }
        }
    }

    /// The subject authorization compares.
    #[must_use]
    pub fn subject(&self) -> Subject {
        Subject { key: self.key(), set: matches!(self, Self::ExternalSet { .. }) }
    }

    /// The principal a key names.
    pub fn from_key(key: &str) -> Result<Self, PrincipalError> {
        let bad = || PrincipalError::BadKey(key.to_owned());
        if key == ANONYMOUS {
            return Ok(Self::Anonymous);
        }
        let (kind, rest) = key.split_once(':').ok_or_else(bad)?;
        let uuid = || Uuid::parse_str(rest).map_err(|_| bad());
        let principal = match kind {
            "user" => Self::User(uuid()?),
            "token" => Self::Token(uuid()?),
            "link" => Self::Link(uuid()?),
            "workload" => Self::Workload(uuid()?),
            "extension" => Self::Extension(uuid()?),
            "system" => Self::System(
                SystemActor::ALL.into_iter().find(|a| a.name() == rest).ok_or_else(bad)?,
            ),
            "ext" => Self::external_from_key(rest).ok_or_else(bad)?,
            _ => return Err(bad()),
        };
        principal.validate().map_err(|_| bad())?;
        Ok(principal)
    }

    fn external_from_key(rest: &str) -> Option<Self> {
        let (provider, rest) = rest.split_once(':')?;
        let provider = provider.to_owned();
        match rest.split_once(':')? {
            ("user", user) => Some(Self::ExternalUser { provider, user: user.to_owned() }),
            ("set", set) => {
                let (realm, member_of) = set.split_once(':')?;
                Some(Self::ExternalSet {
                    provider,
                    realm: realm.to_owned(),
                    member_of: member_of.to_owned(),
                })
            }
            _ => None,
        }
    }
}

impl fmt::Display for Principal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_parses_back_to_its_principal() {
        let all = [
            Principal::User(Uuid::now_v7()),
            Principal::Token(Uuid::now_v7()),
            Principal::Link(Uuid::now_v7()),
            Principal::Workload(Uuid::now_v7()),
            Principal::Extension(Uuid::now_v7()),
            Principal::System(SystemActor::Bootstrap),
            Principal::Anonymous,
            Principal::ExternalUser { provider: "discord".into(), user: "12:34".into() },
            Principal::ExternalSet {
                provider: "discord".into(),
                realm: "guild-456".into(),
                member_of: "role:mod".into(),
            },
        ];
        for p in all {
            assert_eq!(Principal::from_key(&p.key()), Ok(p.clone()), "{p}");
        }
    }

    #[test]
    fn only_external_sets_are_sets_and_ambiguous_parts_are_refused() {
        let set = Principal::ExternalSet {
            provider: "oidc".into(),
            realm: "issuer".into(),
            member_of: "admins".into(),
        };
        assert!(set.subject().set);
        assert!(!Principal::Token(Uuid::now_v7()).subject().set);
        let ambiguous = Principal::ExternalUser { provider: "a:b".into(), user: "c".into() };
        assert_eq!(ambiguous.validate(), Err(PrincipalError::BadPart("provider")));
        assert!(Principal::from_key("token:not-a-uuid").is_err());
        assert!(Principal::from_key("system:janitor").is_err());
    }
}
