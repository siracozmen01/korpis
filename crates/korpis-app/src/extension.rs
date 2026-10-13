// SPDX-License-Identifier: AGPL-3.0-or-later

//! Extensions (docs/16-extensions.md): installing one, reading what is installed, and
//! uninstalling it. An
//! extension is a workload acting as `extension:<id>` under grants attenuated from its
//! installer's, so everything here is built from the workload and grant use cases.

mod grants;
mod install;
mod read;
mod uninstall;

pub use install::{InstallExtension, Installed};
pub use read::{ExtensionStatus, ExtensionView};
pub use uninstall::Uninstalled;
