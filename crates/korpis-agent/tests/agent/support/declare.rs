// SPDX-License-Identifier: AGPL-3.0-or-later

//! Workloads declared from the example recipe.

use super::{ControlPlane, mutation, push_paper};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;

impl ControlPlane {
    /// Declares workload `name` from the example recipe in `project`; its wire id.
    pub async fn declare(&self, project: &str, name: &str) -> String {
        self.declare_with(project, name, Vec::new()).await
    }

    /// Declares workload `name` as [`Self::declare`] does, with `volumes` declared.
    pub async fn declare_with(
        &self,
        project: &str,
        name: &str,
        volumes: Vec<pb::VolumeSpec>,
    ) -> String {
        self.declare_from(project, name, push_paper().await, volumes).await
    }

    /// Declares workload `name` from the recipe pushed to `repo`, at its first target, with
    /// `volumes` declared; its wire id.
    pub async fn declare_from(
        &self,
        project: &str,
        name: &str,
        repo: String,
        volumes: Vec<pb::VolumeSpec>,
    ) -> String {
        // The recipe's game endpoint is `stable`, which without an edge tier needs a node
        // reachable on its own address (§3 of docs/07-networking.md). The test nodes advertise
        // the loopback address, which the control plane's prober reaches; wait until it has.
        self.measured().await;
        let declared = self
            .workloads()
            .declare_intent(pb::DeclareIntentRequest {
                mutation: mutation(&format!("declare-{name}")).into(),
                target: Some(Target::Create(Box::new(pb::NewWorkload {
                    project: project.to_owned(),
                    name: name.to_owned(),
                    ..Default::default()
                }))),
                body: pb::IntentBody {
                    recipe: pb::RecipeRef {
                        name: repo,
                        version: "1.21.4".into(),
                        ..Default::default()
                    }
                    .into(),
                    volumes,
                    ..Default::default()
                }
                .into(),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_owned();
        declared.workload.as_option().unwrap().id.clone()
    }
}
