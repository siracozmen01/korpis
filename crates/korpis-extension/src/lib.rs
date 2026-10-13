// SPDX-License-Identifier: Apache-2.0

//! The SDK a Korpis extension is written with (docs/16-extensions.md). An extension is a
//! workload that acts as its installation: it obtains a short-lived credential from the identity
//! socket its node serves it (§9.5 of docs/08-identity.md), which also says where the control
//! plane is and how to trust it; it follows the event subscriptions its installation holds,
//! acknowledging what it handled (§5 of docs/15-observability.md); and, if it provides anything,
//! it holds a stream the control plane calls it down (§3.3 of docs/16-extensions.md).
//!
//! The SDK is Apache-2.0, like the protocol it speaks (§2 of docs/19-governance.md), so an
//! extension may carry any licence its author chooses. It depends on the wire contract alone.

pub mod consumer;
pub mod dns;
pub mod error;
pub mod identity;
pub mod provider;
pub mod step;

pub use error::Error;
pub use identity::{Connection, Credential, CredentialSource, Fixed, IdentitySocket};
