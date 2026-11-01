// SPDX-License-Identifier: AGPL-3.0-or-later

//! The event log's consumers: subscriptions, and their delivery.

mod deliver;
mod subscriptions;

pub use deliver::{Delivered, Delivery};
