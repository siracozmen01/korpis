// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis address-pool …` (§3.7 of docs/07-networking.md): the addresses an organization's
//! nodes hold for its workloads' `dedicated_ip` endpoints.

use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{short, table, time};
use clap::{Subcommand, ValueEnum};
use korpis_proto::korpis::v1 as pb;

/// How a pool's addresses reach the node holding one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Routing {
    /// The network routes them to one node.
    Static,
    /// A router announces each over BGP.
    Bgp,
    /// A router moves each through its provider's API.
    ProviderApi,
}

/// `korpis address-pool …`.
#[derive(Debug, Clone, Subcommand)]
pub enum AddressPoolAction {
    /// Makes a pool of addresses routed to one node of the organization's, moved between them by
    /// an address router, or held by an edge group's members: an operator's, vouching that the organization holds the range
    /// (`address_pool.attest`). One overlapping any other pool is refused.
    Create {
        /// Its addresses, as a network, e.g. `203.0.113.0/28`.
        cidr: String,
        /// The organization whose pool it is, by name or identifier; the top one you can read by
        /// default.
        #[arg(long)]
        organization: Option<String>,
        /// The node its addresses are routed to, `nd_…`.
        #[arg(
            long,
            required_unless_present_any = ["router", "edge_group"],
            conflicts_with_all = ["router", "edge_group"]
        )]
        node: Option<String>,
        /// The edge group whose members hold its addresses for `stable` endpoints, `egr_…`: on
        /// the link they share with `--interface`, or moved by `--router`.
        #[arg(long)]
        edge_group: Option<String>,
        /// The address router that moves each address to the node holding it: the name of an
        /// `address_router` provider the organization sees.
        #[arg(long, requires = "routing")]
        router: Option<String>,
        /// How a router moves the addresses; a node pool's are static.
        #[arg(long, value_enum)]
        routing: Option<Routing>,
        /// The node's interface its addresses are answered for on, where they are on the node's
        /// link rather than routed to it.
        #[arg(long)]
        interface: Option<String>,
        /// How long a released address rests before it is given again, e.g. `12h`; a day by
        /// default.
        #[arg(long)]
        quarantine: Option<String>,
        /// An address never given, such as the link's gateway; repeat for more.
        #[arg(long = "reserve")]
        reserved: Vec<String>,
    },
    /// The pools of an organization, with how many of each one's addresses are held.
    List {
        /// The organization, by name or identifier.
        #[arg(long)]
        organization: Option<String>,
    },
    /// Deletes a pool none of whose addresses is held.
    Delete {
        /// The pool, `apl_…`.
        pool: String,
    },
}

fn rows(list: &[pb::AddressPool]) -> String {
    let head = ["ID", "CIDR", "SCOPE", "ON", "ROUTING", "HELD", "MADE"];
    let mut all = vec![head.map(str::to_owned).to_vec()];
    all.extend(list.iter().map(|p| {
        let on = [&p.node, &p.group, &p.router].into_iter().find(|s| !s.is_empty());
        let on = on.cloned().unwrap_or_default();
        vec![
            p.id.clone(),
            p.cidr.clone(),
            short(p.scope, "ADDRESS_POOL_SCOPE_").to_lowercase(),
            on,
            short(p.routing, "ADDRESS_ROUTING_").to_lowercase(),
            p.held.to_string(),
            time(p.created_at.as_option()),
        ]
    }));
    table(&all)
}

fn routing_to_pb(r: Routing) -> pb::AddressRouting {
    match r {
        Routing::Static => pb::AddressRouting::ADDRESS_ROUTING_STATIC,
        Routing::Bgp => pb::AddressRouting::ADDRESS_ROUTING_BGP,
        Routing::ProviderApi => pb::AddressRouting::ADDRESS_ROUTING_PROVIDER_API,
    }
}

/// The pool `create` asks for.
async fn asked(c: &Connection, action: &AddressPoolAction) -> Result<pb::AddressPool, CliError> {
    let AddressPoolAction::Create {
        cidr,
        organization,
        node,
        edge_group,
        router,
        routing,
        interface,
        quarantine,
        reserved,
    } = action
    else {
        return Err(CliError::Local("not a create".into()));
    };
    let quarantine = match quarantine {
        Some(q) => Some(korpis_core::quantity::parse_duration(q).map_err(|e| {
            CliError::Local(format!("--quarantine {q:?}: a span such as 12h or 2d ({e})"))
        })?),
        None => None,
    };
    let scope = match (node, edge_group) {
        (Some(_), _) => pb::AddressPoolScope::ADDRESS_POOL_SCOPE_NODE,
        (None, Some(_)) => pb::AddressPoolScope::ADDRESS_POOL_SCOPE_EDGE,
        (None, None) => pb::AddressPoolScope::ADDRESS_POOL_SCOPE_CLUSTER,
    };
    Ok(pb::AddressPool {
        organization: resolve::organization(c, organization.as_deref()).await?,
        cidr: cidr.clone(),
        scope: scope.into(),
        node: node.clone().unwrap_or_default(),
        router: router.clone().unwrap_or_default(),
        group: edge_group.clone().unwrap_or_default(),
        routing: routing.map(routing_to_pb).unwrap_or_default().into(),
        interface: interface.clone().unwrap_or_default(),
        quarantine: quarantine.map(korpis_wire::duration).into(),
        reserved: reserved.clone(),
        ..Default::default()
    })
}

/// Answers `korpis address-pool …`.
pub async fn address_pool(c: &Connection, action: &AddressPoolAction) -> Result<Answer, CliError> {
    let client = pb::AddressPoolServiceClient::new(c.http(), c.config());
    Ok(match action {
        AddressPoolAction::Create { .. } => {
            let r = pb::CreateAddressPoolRequest {
                mutation: mutation(None).into(),
                pool: asked(c, action).await?.into(),
                ..Default::default()
            };
            let made = client.create_address_pool(r).await?.into_owned().pool;
            let pool = made.into_option().unwrap_or_default();
            Answer { human: rows(std::slice::from_ref(&pool)), json: json(&pool) }
        }
        AddressPoolAction::List { organization } => {
            let r = pb::ListAddressPoolsRequest {
                organization: resolve::organization(c, organization.as_deref()).await?,
                ..Default::default()
            };
            let list = client.list_address_pools(r).await?.into_owned().pools;
            Answer { human: rows(&list), json: json(&list) }
        }
        AddressPoolAction::Delete { pool } => {
            let r = pb::DeleteAddressPoolRequest {
                mutation: mutation(None).into(),
                id: pool.clone(),
                ..Default::default()
            };
            client.delete_address_pool(r).await?;
            Answer { human: format!("deleted {pool}\n"), json: serde_json::json!({}) }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pool_is_shown_with_where_its_addresses_are_and_how_many_are_held() {
        let p = pb::AddressPool {
            id: "apl_1".into(),
            cidr: "203.0.113.0/28".into(),
            scope: pb::AddressPoolScope::ADDRESS_POOL_SCOPE_NODE.into(),
            node: "nd_1".into(),
            routing: pb::AddressRouting::ADDRESS_ROUTING_STATIC.into(),
            held: 3,
            ..Default::default()
        };
        let human = rows(&[p]);
        assert!(human.contains("203.0.113.0/28") && human.contains("nd_1"), "{human}");
        assert!(human.contains("node") && human.contains("static"), "{human}");
    }
}
