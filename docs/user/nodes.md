# Nodes and relays

How to enroll a node, run its agent, drivers and helper, remove it, and enroll a relay. What a node
needs is on [install](install.md).

## Contents

- [Enrolling a node](#enrolling-a-node)
- [Running the agent](#running-the-agent)
- [The drivers](#the-drivers)
- [The helper](#the-helper)
- [Removing a node](#removing-a-node)
- [Enrolling a relay](#enrolling-a-relay)
- [Serving SFTP](#serving-sftp)

## Enrolling a node

The agent dials out, so enrolling opens nothing: no inbound firewall rule, NAT irrelevant, a node
behind a residential connection an ordinary node.

```
korpis node token create fra-3 --label region=eu --expires 1h
-> korpis-agent join --server https://korpis.example:7443 --token korpis_8f2a... \
      --ca-fingerprint sha256:4c1a...
```

Creating a token needs `node.enroll` over the organization the node will belong to. The token is
single-use, lives an hour by default and never more than a day, and binds the node's name and
labels, so the node enrolled is the one you described whatever the agent claims; its secret is shown
once and never stored. The agent generates an Ed25519 key, writes it before sending it so a join
whose answer was lost retries with the same key, verifies the control plane against the CA
fingerprint the command carries, and trusts that CA alone from then on. A spent token whose
enrollment failed is not given back: create another.

## Running the agent

`korpis-agent run` is the node. It starts whether or not the control plane is reachable, since a
node keeps doing its job from what it holds, and stopping it (for an upgrade or by `SIGTERM`) leaves
every workload as it is for the next start to adopt.

| Setting | Meaning |
|---|---|
| `--driver-socket` (repeatable) | each driver the node runs; `/run/korpis/driver-oci.sock` by default |
| `--helper-socket` | `/run/korpis/helper.sock` |
| `--advertise-address` (repeatable) | an address clients reach the node at that it cannot see itself; measured before relied on |
| `--ports` | the endpoint port range, 30000-32767 by default, never below 1024 |
| `--ingress false` | serve no ingress; otherwise the helper opens 80 and 443 and starts `korpis-ingress` |
| `--fetch-allow` (repeatable CIDR) | a private network a recipe's `fetch` or a registry may reach |
| `--plain-http` | a development registry over plain HTTP |
| `--uplink eth0=1gbit[,reserve=50mbit]` | each uplink and the capacity it sells |
| `--overlay-port` | the mesh's UDP port, 51820 by default |
| `--workload-ipv4`, `--workload-ipv6` | the node's workload pools; naming an IPv4 pool takes the node out of the mesh |
| `--dedicated-cores`, `--cdi-dir`, `--device-class`, `--device-share` | cores and devices offered |

`--driver memory` runs no driver, for trying the agent alone. `korpis-helper release-networks`
releases every workload network left under the namespace root, for cleaning a machine whose node was
removed.

## The drivers

**`korpis-driver-oci`** runs as its own user with access to containerd's socket and nothing else the
agent holds, serving `/run/korpis/driver-oci.sock` to the users `--allow-uid` names. It reads images
from the agent's content store and mounts only volumes under the helper's volume root, a
`--shared-root`, or the block root, and each workload's identity directory under
`/var/lib/korpis-agent/identity`; `--socket`, `--content`, `--volume-root`, `--identity-root` and
`--containerd` move them, and a containerd whose state directory is not `/run/containerd` needs
`--containerd-state`. Its containers live under `--cgroup-parent` (`/korpis`). Stopping it stops
nothing; the next one recovers the containers.

**`korpis-driver-firecracker`** runs as root, because the jailer must, and serves
`/run/korpis/driver-firecracker.sock`, given to the agent as a second `--driver-socket`. No guest
runs as root: each runs as its workload's first id. It reads Firecracker, the jailer, the kernel,
`korpis-guest`, `korpis-fsworker` and `korpis-install-job` from `/usr/lib/korpis` (`--firecracker`,
`--jailer`, `--kernel`, `--guest`, `--fsworker`, `--install-job`), keeps its state in
`/var/lib/korpis-driver-firecracker`, and its guests' cgroups under `korpis-firecracker`. Its unit
runs under `ProtectSystem=strict` with `KillMode=process`, since guests outlive it, and without
`RestrictSUIDSGID`, under which systemd refuses `openat2`. An upgraded package reaches each guest at
its next boot. Both drivers take `--dedicated-cores-host`.

## The helper

The helper runs only when asked: a systemd socket unit (`/run/korpis/helper.sock`, mode `0660`, the
agent's group) starts `korpis-helper --inherit`, which serves whoever connects among the users
`--allow-uid` names and exits after a minute idle; without systemd, `--socket PATH` binds it. Its
default class is `local` under `--volume-root` (`/var/lib/korpis/volumes`), its backend read from
the filesystem there; its state under `--state` (`/var/lib/korpis/helper`). Further classes:

- `--shared-class NAME=PATH`: a network filesystem you mounted on every node offering it. Every
  client writing to that export as root is trusted as a node is, so export it to the installation's
  nodes alone.
- `--block-class NAME=iscsi|rbd|loop:DIR`: served by the `block_storage` provider of that name the
  node's organization sees, mounted under `--block-root` (`/var/lib/korpis/block`).

Jobs are binaries beside it (`--job-binary`, `--install-binary`, `none` to run no installs), their
cgroups under `--job-cgroups` (`/sys/fs/cgroup/korpis/shared`, so jobs keep off dedicated cores). It
keeps dedicated-core cpusets only under the cgroup parents `--cores-cgroup` names (`/korpis` and
`/korpis-firecracker` by default).

## Removing a node

Removing is never abrupt: cordon, drain, then remove, each producing a plan that names every
workload and where it goes. `korpis node remove` (the irreversible `node.remove`) is refused unless
the node is cordoned and holds no workload and no undeleted volume; the node is then decommissioned
for good and its key opens nothing. A node removed as compromised (`--compromised`) is removed
whatever it holds: its workloads are placed again from their targets, and the certificates whose
keys it held are revoked and renewed. The CLI asks for `--yes` either way.

What the node made on its machine is not removed with it: its containers (containerd's `korpis`
namespace), its volumes (the helper's volume root) and its workloads' networks (`korpis-helper
release-networks`) stay until you remove them.

## Enrolling a relay

A relay running as its own process carries every organization's sessions, so its token is created
over the root organization alone (`NodeService.CreateRelayEnrollmentToken`), binding the relay's
name and the URL clients and nodes reach it at. The relay joins with `korpis-server relay join
--server ... --token ... --ca-fingerprint ...`, presenting an Ed25519 identity key and a P-256 TLS
key it generated, and is given a certificate for its URL's host from the installation's authority,
which agents and the CLI already trust. It renews the certificate itself at about two thirds of its
life, with no restart. `korpis-server relay run` serves it (`--listen` overrides the address); it
stops once the control plane refuses its feed. `ListRelays` shows each relay's certificate expiry
and connection; `RemoveRelay` removes one, whose certificate stays valid until it expires.

Behind a load balancer, `relay run` and `serve` take `--proxy-protocol-from` with the balancer's
addresses, which must then send a PROXY header on every connection. Where relays carry the sessions,
`korpis-server serve --relay-role false` takes the role off the control plane.

## Serving SFTP

SFTP is off until you turn it on, relay by relay, because a relay serving it can open the files of
anyone with a registered SSH key ([security](../internals/security.md#what-the-operator-can-see)).
`--sftp-listen ADDRESS:PORT` on `relay run` (or `serve`), `--sftp-host-key PATH` (made at first
start, readable by the server alone), and `--sftp-advertise HOST:PORT` where clients reach it at
another address. The relay reports the address and the key's fingerprint, and `ServerInfo` lists
them, so the panel and `korpis sftp` tell a person where to connect and which key to expect. Give
several relays behind one name the same key file, or clients see a changed host key. Each change is
recorded (`relay.sftp_reported`, `relay.sftp_withdrawn`), so a replaced key is on the record before
a client warns of it.
