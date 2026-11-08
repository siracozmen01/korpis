// SPDX-License-Identifier: AGPL-3.0-or-later

//! Extensions: installing one, reading what is installed, uninstalling it, and the providers
//! installations serve. An extension is a workload acting as `extension:<id>` under grants
//! attenuated from its installer's, so everything here is built from the workload and grant use
//! cases.

mod breaker;
mod call;
mod grants;
mod install;
mod invoke;
mod policy;
mod provider;
mod read;
mod slots;
mod uninstall;
mod upgrade;

pub use breaker::ProviderHealth;
pub use call::{CallTarget, Called};
pub use install::{InstallExtension, Installed};
pub use invoke::{ACTION_WITHIN, InvokeAction, InvokedAction};
pub use policy::{GoverningExtensions, SetPolicy};
pub use provider::Register;
pub use read::{ExtensionStatus, ExtensionView};
pub use slots::SlotShown;
pub use uninstall::Uninstalled;
pub use upgrade::{UpgradeExtension, Upgraded};
