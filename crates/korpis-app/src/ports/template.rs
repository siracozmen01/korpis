// SPDX-License-Identifier: AGPL-3.0-or-later

//! Grant templates (§4 of docs/08-identity.md): a head naming the current version, and every
//! version kept whole beside it.

use super::StoreResult;
use crate::page::Page;
use korpis_core::authz::template::{GrantTemplate, TemplateVersion};
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// Reading templates.
pub trait TemplateRead {
    /// A template at its current version.
    fn template(&self, id: Uuid)
    -> impl Future<Output = StoreResult<Option<GrantTemplate>>> + Send;

    /// One version of a template, as it was made.
    fn template_version(
        &self,
        id: Uuid,
        version: u64,
    ) -> impl Future<Output = StoreResult<Option<TemplateVersion>>> + Send;

    /// The templates the organizations `owners` own, by identifier; only the live ones when
    /// `live`.
    fn templates_of(
        &self,
        owners: &[Uuid],
        live: bool,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<GrantTemplate>>> + Send;
}

/// Writing templates. A live template's name is unique in its organization, refused as
/// `Duplicate(Unique::TemplateName)`.
pub trait TemplateWrite {
    /// Inserts a template and its first version.
    fn insert_template(
        &mut self,
        template: &GrantTemplate,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Makes `version` the current version of live template `id`, if its current version is
    /// still `expected`; false otherwise.
    fn update_template(
        &mut self,
        id: Uuid,
        expected: u64,
        version: &TemplateVersion,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Retires a live template at `at`; false if it was retired already.
    fn retire_template(
        &mut self,
        id: Uuid,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}
