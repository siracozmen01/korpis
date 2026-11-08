// SPDX-License-Identifier: AGPL-3.0-or-later

//! Invoking an extension's action, `ext.<name>.<action>`. The invoker is authorized for the action
//! on each object its arguments name, or on where the extension is installed when they name none,
//! and the extension is sent the invocation with a token it presents beside its own credential.
//! With that token it acts with the intersection of its own grants and the invoker's authorization:
//! the core actions its grants permit, only on those objects and what lies beneath them, so an
//! invoker authorized on one workload cannot have it act on another (the confused deputy). The
//! token is accepted only until the invocation finishes, and the invocation is an effect naming
//! the invoker, the extension and the action.

use super::call::CallTarget;
use crate::App;
use crate::caller::{Caller, Invoked};
use crate::error::AppError;
use crate::ports::{Clock, InvocationWrite as _, RecipeSource, Store, Tx as _};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::credential::{Secret, SecretKind};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::extension::action::{self, ActionResult, Arguments, Invocation, Invoke};
use korpis_core::extension::{CallFailure, CallOutcome, CallRequest, Installation, ProviderType};
use std::time::Duration;
use uuid::Uuid;

/// How long an extension has to perform an action, and how long its token lives at most.
pub const ACTION_WITHIN: Duration = Duration::from_secs(60);

/// What invoking an action asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvokeAction {
    /// The installation.
    pub extension: Uuid,
    /// The part after `ext.<name>.`.
    pub action: String,
    /// Its arguments.
    pub arguments: Arguments,
}

/// What an invocation came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvokedAction {
    /// The invocation, as its effect and the effects made under it name it.
    pub invocation: Uuid,
    /// What the action answered, or why it did not.
    pub result: Result<ActionResult, CallFailure>,
}

/// Where `i` is installed: what an action taking no resource is authorized and acts on.
const fn installed_at(i: &Installation) -> ObjectRef {
    match i.project {
        Some(p) => ObjectRef::new(Kind::Project, p),
        None => ObjectRef::new(Kind::Organization, i.organization),
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Invokes one of an installation's actions for `caller`.
    ///
    /// # Errors
    /// [`AppError::NotFound`] for an installation the caller may not read, or an action it does
    /// not declare; [`AppError::Invalid`] for arguments that do not fit it; a refusal for an
    /// object the caller is not authorized on for the action.
    pub async fn invoke_action(
        &self,
        caller: &Caller,
        asked: InvokeAction,
    ) -> Result<InvokedAction, AppError> {
        caller.direct()?;
        let installation = self.store.extension(asked.extension).await?;
        let installation = installation
            .filter(Installation::is_installed)
            .ok_or(AppError::NotFound { kind: "extension" })?;
        let at = installed_at(&installation);
        self.authorize(caller, "extension.read", at).await?;
        let manifest = self.manifest_of(&installation).await?;
        let declared = manifest
            .actions
            .iter()
            .find(|a| a.name == asked.action)
            .ok_or(AppError::NotFound { kind: "action" })?;
        action::check(&declared.arguments, &asked.arguments).map_err(|e| {
            AppError::invalid(format!("arguments.{}", e.argument), e.reason.clone())
        })?;
        let full = format!("ext.{}.{}", installation.name, asked.action);
        let mut resources = action::resources(&asked.arguments);
        if resources.is_empty() {
            resources.push(at);
        }
        for (i, object) in resources.iter().enumerate() {
            // An installation acts only where it is installed, whatever else an invoker's grant
            // of an action of this name reaches.
            if !self.lies_within(*object, at).await? {
                let why = "an object where the extension is installed";
                return Err(AppError::invalid(format!("arguments[{i}]"), why));
            }
            self.authorize(caller, &full, *object).await?;
        }
        let (invocation, secret) = self.begin_invocation(caller, &installation, &full, resources).await?;
        let request = CallRequest::InvokeAction(Invoke {
            action: asked.action,
            arguments: asked.arguments,
            invocation: secret.expose().to_owned(),
            invoker: caller.principal.key(),
        });
        let target = CallTarget {
            organization: installation.organization,
            kind: ProviderType::Action,
            name: installation.id.to_string(),
        };
        let called = self.call_own_provider(&target, request, ACTION_WITHIN).await;
        let result = match called.map(|c| c.outcome) {
            Ok(CallOutcome::ActionResult(r)) => Ok(r),
            Ok(CallOutcome::Failed(f)) => Err(f),
            Ok(_) => Err(CallFailure::malformed()),
            Err(e) => {
                self.end_invocation(caller, (&invocation, at), &Err(CallFailure::deadline())).await?;
                return Err(e);
            }
        };
        self.end_invocation(caller, (&invocation, at), &result).await?;
        Ok(InvokedAction { invocation: invocation.id, result })
    }

    /// Records the invocation and mints its token.
    async fn begin_invocation(
        &self,
        caller: &Caller,
        installation: &Installation,
        full: &str,
        resources: Vec<ObjectRef>,
    ) -> Result<(Invocation, Secret), AppError> {
        let now = self.clock.now();
        let secret = Secret::generate_kind(SecretKind::Invocation)
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let invocation = Invocation {
            id: Uuid::now_v7(),
            extension: installation.id,
            action: full.to_owned(),
            invoker: caller.principal.key(),
            resources,
            expires_at: now + ACTION_WITHIN,
        };
        let mut tx = self.store.begin().await?;
        tx.insert_invocation(&invocation, &secret.digest(), now).await?;
        tx.commit().await?;
        Ok((invocation, secret))
    }

    /// Finishes the invocation, so its token is accepted no more, and records it as an effect by
    /// the invoker, about where the extension is installed, naming it and what it answered.
    async fn end_invocation(
        &self,
        caller: &Caller,
        (invocation, at): (&Invocation, ObjectRef),
        result: &Result<ActionResult, CallFailure>,
    ) -> Result<(), AppError> {
        let outcome = if result.is_ok() { Outcome::Succeeded } else { Outcome::Failed };
        let mut effect = caller
            .effect("extension.invoke", outcome)
            .about(at)
            .invoked(Some(invocation.id))
            .after(details([
                ("action", invocation.action.clone().into()),
                ("extension", invocation.extension.to_string().into()),
            ]));
        if let Err(f) = result {
            effect.error = Some(details([
                ("reason", f.reason.clone().into()),
                ("message", f.message.clone().into()),
            ]));
        }
        let mut tx = self.store.begin().await?;
        tx.finish_invocation(invocation.id, self.clock.now()).await?;
        tx.append_effect(&effect).await?;
        tx.commit().await?;
        Ok(())
    }

    /// The caller an extension's call acts as when it presents an invocation's token beside its
    /// own credential: itself, narrowed to the objects the invoker was authorized on. A token
    /// that is unknown, finished, expired, or another installation's is not accepted.
    ///
    /// # Errors
    /// [`AppError::Unauthenticated`] for such a token.
    pub async fn invoked(&self, extension: &Caller, presented: &str) -> Result<Caller, AppError> {
        let secret = Secret::presented_as(SecretKind::Invocation, presented)
            .ok_or(AppError::Unauthenticated)?;
        let live = self.store.live_invocation(&secret.digest(), self.clock.now()).await?;
        let invocation = live.ok_or(AppError::Unauthenticated)?;
        let ours = korpis_core::principal::Principal::Extension(invocation.extension);
        if extension.principal != ours || extension.relayed.is_some() {
            return Err(AppError::Unauthenticated);
        }
        let mut caller = extension.clone();
        caller.invoked = Some(Invoked {
            invocation: invocation.id,
            invoker: invocation.invoker,
            resources: invocation.resources,
        });
        Ok(caller)
    }

    /// Forgets invocations finished or expired more than an hour ago; how many.
    pub async fn forget_invocations(&self) -> Result<u64, AppError> {
        let before = self.clock.now() - Duration::from_secs(3600);
        let mut tx = self.store.begin().await?;
        let n = tx.forget_invocations(before).await?;
        tx.commit().await?;
        Ok(n)
    }
}
