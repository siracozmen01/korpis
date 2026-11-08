// SPDX-License-Identifier: Apache-2.0

//! The SDK a Korpis extension is written with. An extension is a workload that acts as its
//! installation: it obtains a short-lived credential from the identity socket its node serves it,
//! which also says where the control plane is and how to trust it; it follows the event
//! subscriptions its installation holds, acknowledging what it handled; and, if it provides
//! anything, it holds a stream the control plane calls it down.
//!
//! The SDK is Apache-2.0, like the protocol it speaks, so an extension may carry any licence its
//! author chooses. It depends on the wire contract alone.

pub mod action;
pub mod address;
pub mod backup;
pub mod block;
pub mod consumer;
pub mod dns;
pub mod error;
pub mod health;
pub mod identity;
pub mod identity_provider;
pub mod keys;
pub mod keyseal;
pub mod provider;
pub mod sigv4;
pub mod step;
pub mod transport;

pub use error::Error;
pub use identity::{Connection, Credential, CredentialSource, Fixed, IdentitySocket};
pub use transport::Transport;
