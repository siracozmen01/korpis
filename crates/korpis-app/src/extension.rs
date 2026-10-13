// SPDX-License-Identifier: AGPL-3.0-or-later

//! Extensions (docs/16-extensions.md): installing one, and reading what is installed. An
//! extension is a workload acting as `extension:<id>` under grants attenuated from its
//! installer's, so everything here is built from the workload and grant use cases.

mod grants;
mod install;
mod read;

pub use install::{InstallExtension, Installed};
pub use read::{ExtensionStatus, ExtensionView};
