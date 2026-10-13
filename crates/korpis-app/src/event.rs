// SPDX-License-Identifier: AGPL-3.0-or-later

//! The event log's consumers (§5 of docs/15-observability.md): subscriptions, and their
//! delivery.

mod deliver;
mod subscriptions;

pub use deliver::{Delivered, Delivery};
