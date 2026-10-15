// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's network, made before its runtime object is created and released once the
//! workload has left the node with nothing of it left here (§4.1 of docs/07-networking.md).

use super::Reconciler;
use super::act::Failure;
use crate::held::HeldSet;
use crate::host::Host;
use crate::runtime::Runtime;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::driver::v1 as driver;

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Makes `a`'s workload's network and has `spec` join it, where the node gives networks;
    /// its namespaces map the uid range the assignment gives it.
    pub(super) async fn network(
        &mut self,
        a: &agent::Assignment,
        spec: &mut driver::PreparedSpec,
    ) -> Result<(), Failure> {
        let Some(net) = self.wiring.network.as_mut() else { return Ok(()) };
        let assigned = net
            .addresses
            .assign(&a.workload, net.pools)
            .map_err(|e| Failure(format!("the workload's addresses: {e}")))?;
        let mut request = net.request(&a.workload, assigned);
        (request.uid_base, request.uid_count) = (a.uid_base, a.uid_count);
        let made = self.host.ensure_network(&request).await?;
        spec.namespaces = made.namespaces;
        Ok(())
    }

    /// Releases the network of every workload holding addresses that the set no longer holds
    /// and nothing tracks; one that cannot be released now is tried again at the next pass.
    pub(super) async fn release_networks(&mut self, held: &HeldSet) {
        let Some(net) = self.wiring.network.as_ref() else { return };
        let leaving: Vec<String> = net
            .addresses
            .workloads()
            .filter(|w| !held.assignments.contains_key(*w) && !self.tracked.contains_key(*w))
            .cloned()
            .collect();
        for workload in leaving {
            if let Err(e) = self.host.release_network(&workload).await {
                tracing::warn!(%workload, error = %e, "a network could not be released yet");
                continue;
            }
            let freed = self.wiring.network.as_mut().map(|n| n.addresses.release(&workload));
            if let Some(Err(e)) = freed {
                tracing::error!(%workload, error = %e, "the addresses could not be kept");
            }
        }
    }
}
