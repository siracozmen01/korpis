// SPDX-License-Identifier: AGPL-3.0-or-later

//! TXT lookups for verifying domains (§3.2 of docs/07-networking.md), through the system's
//! resolvers or one the operator names. Nothing is cached, so a holder who publishes the record
//! and asks again is answered from DNS as it is now, not as it was.

use hickory_resolver::TokioResolver;
use hickory_resolver::config::{ConnectionConfig, NameServerConfig, ResolverConfig};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::proto::rr::RData;
use korpis_app::ports::{LookupError, TxtLookup};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;

/// TXT lookups through a resolver.
#[derive(Debug)]
pub struct Lookup(TokioResolver);

impl Lookup {
    /// Lookups through `server` over UDP and TCP, or through the system's resolvers without one.
    ///
    /// # Errors
    /// The system's resolver configuration cannot be read.
    pub fn new(server: Option<SocketAddr>) -> Result<Self, String> {
        let mut builder = match server {
            Some(at) => {
                let connections = [ConnectionConfig::udp(), ConnectionConfig::tcp()]
                    .into_iter()
                    .map(|mut c| {
                        c.port = at.port();
                        c
                    })
                    .collect();
                let ns = NameServerConfig::new(at.ip(), true, connections);
                let config = ResolverConfig::from_name_servers(vec![ns]);
                TokioResolver::builder_with_config(config, TokioRuntimeProvider::default())
            }
            None => TokioResolver::builder_tokio().map_err(|e| e.to_string())?,
        };
        builder.options_mut().cache_size = 0;
        Ok(Self(builder.build().map_err(|e| e.to_string())?))
    }
}

impl TxtLookup for Lookup {
    fn txt<'a>(
        &'a self,
        name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, LookupError>> + Send + 'a>> {
        Box::pin(async move {
            let fqdn = format!("{name}.");
            let found = match self.0.txt_lookup(fqdn.as_str()).await {
                Ok(l) => l,
                Err(e) if e.is_no_records_found() => return Err(LookupError::NotFound),
                Err(e) => return Err(LookupError::Failed(e.to_string())),
            };
            Ok(found
                .answers()
                .iter()
                .filter_map(|r| match &r.data {
                    RData::TXT(t) => Some(
                        t.txt_data.iter().map(|s| String::from_utf8_lossy(s)).collect::<String>(),
                    ),
                    _ => None,
                })
                .collect())
        })
    }
}
