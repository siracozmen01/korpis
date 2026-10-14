// SPDX-License-Identifier: Apache-2.0

//! Writing a key management provider (§5.5 of docs/06-storage.md): the tenant's own key
//! management service holding a repository's key-encryption key. It makes a workload's data key
//! and wraps it, and opens a wrapped one, for one context, `<repository>/<workload>`, which it
//! should bind into the wrapping and its service's policy, so a credential taken from it opens
//! nothing else. It answers with the data key sealed to the call's one-time recipient
//! ([`crate::keyseal`]), which this module does: an implementation returns the key, and it never
//! leaves the process in the clear. Requires the `provider.key_management` capability.

use crate::keyseal::{self, KEY_LEN};
pub use crate::provider::Failure;
use crate::provider::{Handler, Outcome, Unsupported};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::provider_call::Request;
use std::future::Future;

/// A data key in the clear, as an implementation hands it to this module.
pub type DataKey = [u8; KEY_LEN];

/// A key management service.
pub trait KeyManagement: Send + Sync + 'static {
    /// A new data key for `context`, and its form wrapped by the tenant's `key`.
    fn generate(
        &self,
        key: String,
        context: Vec<u8>,
    ) -> impl Future<Output = Result<(DataKey, Vec<u8>), Failure>> + Send;

    /// The data key `wrapped` holds, opened by the tenant's `key` for `context`.
    fn unwrap(
        &self,
        key: String,
        context: Vec<u8>,
        wrapped: Vec<u8>,
    ) -> impl Future<Output = Result<DataKey, Failure>> + Send;
}

/// Serves a [`KeyManagement`] as a provider's [`Handler`], sealing every key it answers with to
/// the call's recipient.
#[derive(Debug, Clone)]
pub struct Keys<P>(pub P);

fn sealed(recipient: &[u8], mut key: DataKey, context: &[u8]) -> Result<Vec<u8>, Failure> {
    let sealed = keyseal::seal(recipient, &key, context);
    key.fill(0);
    sealed.map_err(|e| Failure {
        reason: "keys.recipient".into(),
        message: e.to_string(),
        retryable: false,
    })
}

fn material(wrapped: Vec<u8>, sealed: Vec<u8>) -> Outcome {
    Outcome::KeyMaterial(Box::new(pb::KeyMaterial { wrapped, sealed, ..Default::default() }))
}

impl<P: KeyManagement> Handler for Keys<P> {
    async fn call(&self, call: pb::ProviderCall) -> Outcome {
        let answered = match call.request {
            Some(Request::GenerateKey(g)) => {
                match self.0.generate(g.key.clone(), g.context.clone()).await {
                    Ok((key, wrapped)) => {
                        sealed(&g.recipient, key, &g.context).map(|s| material(wrapped, s))
                    }
                    Err(f) => Err(f),
                }
            }
            Some(Request::UnwrapKey(u)) => {
                let opened = self.0.unwrap(u.key.clone(), u.context.clone(), u.wrapped.clone());
                match opened.await {
                    Ok(key) => {
                        sealed(&u.recipient, key, &u.context).map(|s| material(Vec::new(), s))
                    }
                    Err(f) => Err(f),
                }
            }
            _ => return Unsupported.call(call).await,
        };
        answered.unwrap_or_else(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyseal::Recipient;

    /// A service whose wrapping is the key reversed, which no real one is.
    struct Mirror;

    impl KeyManagement for Mirror {
        fn generate(
            &self,
            _: String,
            _: Vec<u8>,
        ) -> impl Future<Output = Result<(DataKey, Vec<u8>), Failure>> + Send {
            let key = [5; KEY_LEN];
            std::future::ready(Ok((key, key.iter().rev().copied().collect())))
        }

        fn unwrap(
            &self,
            _: String,
            _: Vec<u8>,
            wrapped: Vec<u8>,
        ) -> impl Future<Output = Result<DataKey, Failure>> + Send {
            let key: Vec<u8> = wrapped.into_iter().rev().collect();
            std::future::ready(key.try_into().map_err(|_| Failure {
                reason: "keys.malformed".into(),
                message: String::new(),
                retryable: false,
            }))
        }
    }

    fn call(request: Request) -> pb::ProviderCall {
        pb::ProviderCall { request: Some(request), ..Default::default() }
    }

    #[tokio::test]
    async fn every_key_answered_with_is_sealed_to_the_call_s_recipient() {
        let recipient = Recipient::new().unwrap();
        let generate = pb::GenerateKey {
            key: "k".into(),
            context: b"r/w".to_vec(),
            recipient: recipient.public().to_vec(),
            ..Default::default()
        };
        let Outcome::KeyMaterial(made) =
            Keys(Mirror).call(call(Request::GenerateKey(Box::new(generate)))).await
        else {
            panic!("no key material");
        };
        assert_eq!(recipient.open(&made.sealed, b"r/w").unwrap(), [5; KEY_LEN]);
        assert!(!made.sealed.windows(KEY_LEN).any(|w| w == [5; KEY_LEN]), "never in the clear");
        let unwrap = pb::UnwrapKey {
            key: "k".into(),
            context: b"r/w".to_vec(),
            recipient: recipient.public().to_vec(),
            wrapped: made.wrapped.clone(),
            ..Default::default()
        };
        let Outcome::KeyMaterial(opened) =
            Keys(Mirror).call(call(Request::UnwrapKey(Box::new(unwrap)))).await
        else {
            panic!("no key material");
        };
        assert!(opened.wrapped.is_empty());
        assert_eq!(recipient.open(&opened.sealed, b"r/w").unwrap(), [5; KEY_LEN]);
    }
}
