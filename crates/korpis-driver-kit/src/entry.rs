// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a workload starts as, from its spec and its image's configuration: the same for every
//! tier, so an image runs one way whether a container or a guest runs it (§6 of
//! docs/04-runtimes.md).

use korpis_core::content::image::ImageConfig;
use korpis_proto::korpis::driver::v1 as driver;

/// The command: the spec's if it names one, else the image's entrypoint, with the spec's
/// arguments if it has any and the image's otherwise.
#[must_use]
pub fn argv(spec: &driver::PreparedSpec, image: &ImageConfig) -> Vec<String> {
    if !spec.command.is_empty() {
        return spec.command.iter().chain(&spec.args).cloned().collect();
    }
    let args = if spec.args.is_empty() { &image.cmd } else { &spec.args };
    image.entrypoint.iter().chain(args).cloned().collect()
}

/// The image's environment, with the spec's variables replacing any of the same name.
#[must_use]
pub fn environment(spec: &driver::PreparedSpec, image: &ImageConfig) -> Vec<String> {
    let own = |e: &&String| e.split_once('=').is_none_or(|(k, _)| !spec.env.contains_key(k));
    image
        .env
        .iter()
        .filter(own)
        .cloned()
        .chain(spec.env.iter().map(|(k, v)| format!("{k}={v}")))
        .collect()
}

/// Where the workload starts: the spec's, else the image's, else the root.
#[must_use]
pub fn working_dir(spec: &driver::PreparedSpec, image: &ImageConfig) -> String {
    [&spec.working_dir, &image.working_dir]
        .into_iter()
        .find(|d| !d.is_empty())
        .cloned()
        .unwrap_or_else(|| "/".to_owned())
}
