# Backup keys and format

Who holds which key and credential for a [backup](backup.md), the backup target providers that issue
access, and what a repository holds.

## Contents

- [Keys](#keys)
- [Custody](#custody)
- [The vault](#the-vault)
- [Backup targets](#backup-targets)
- [Targets that serve their store](#targets-that-serve-their-store)
- [Format](#format)

## Keys

The flow encrypts on the node, deduplicates before the network and streams without staging, and each
needs something on the node: a key, the chunk index, a credential that can write. Given for the
whole repository, a compromised node would reach every workload's backups in it. So keys form a
hierarchy and a node holds only the bottom of it, briefly:

```
repository key-encryption key     held by the control plane, or by a key service
  workload data key               one per workload, made at its first backup, stored only wrapped
    chunk and tree identifiers    keyed hashes: equal content in two workloads is unrelated
    object encryption
```

- **For each job, the node receives one workload's data key**, opened for that job, never the
  key-encryption key.
- **For the same job, it receives a short-lived credential for that workload's prefix alone**: an
  STS session policy on S3-compatible stores, a downscoped token on GCS, a user-delegation SAS on
  Azure, a prefix-restricted application key on B2. It puts, gets and lists there, to deduplicate
  against the workload's history and read its parent's record and trees, and cannot delete.
- **Where a store cannot issue such a credential**, its provider serves the store itself (below), so
  the node writes to the provider and the provider, which never holds a data key, moves ciphertext
  it cannot read.
- **A compromised node therefore reaches the backups of the workloads placed on it**, whose live
  data it could already read, for the length of its jobs, and nothing in any other prefix, which is
  the blast radius stated on [security](security.md).

## Custody

Who holds the key-encryption key is a declared choice per repository, because it decides who can
read tenants' backups.

| `key_custody` | Held by | The operator reads a tenant's backups | Restore needs |
|---|---|---|---|
| `operator` (default) | the control plane | through an explicit grant naming the tenant, visible to the tenant | nothing extra |
| `tenant` | a key service the tenant controls, which makes each data key and opens it each time | only through the tenant's service: every opening is an unwrap it performs, may refuse and logs; revoking the key ends every opening, the operator's included | the tenant's service reachable |

Tenant custody does not make the operator unable to read a backup: the operator runs the nodes, a
job runs there with the data key in memory as the live volume is on disk, and the `key_management`
provider is an extension the operator's nodes run, holding a credential for the tenant's service the
operator could take. What it gives is a key opened only by the tenant's own service, each time, so
every opening is in the tenant's log, and a tenant who revokes one key makes every backup under it
unreadable (crypto-shredding). That is a real and checkable property.

A repository made with `--kms-provider` and `--kms-key` names the organization's `key_management`
provider and the tenant's key, and has no key-encryption key here. A workload's data key is made by
the tenant's service at its first backup and kept wrapped by the tenant's key, the control plane
storing that wrapping alone. Each job asks for its key naming a one-time X25519 public key it made,
and the service answers with the key sealed to it (HPKE, RFC 9180), bound to
`<repository>/<workload>`, so the key is in the clear only in the job's node and the tenant's
service, never in the control plane, its database or its call queue. When the control plane reads a
backup itself (browsing, a file, pruning, offloaded segments), it makes such a recipient in memory
for that read. A service that refuses or cannot be reached fails closed: the job is refused and
asked again later. The cost: every backup, restore, browse, read and prune is a call to the tenant's
service, and a revoked key loses every backup under it, which is the point. A tenant's service
should bind each unwrap to the context it is asked for, so a taken credential opens one workload's
keys at a time, each in its log.

## The vault

With `operator` custody the control plane's vault holds the hierarchy: a backup key in its data
directory (`keys/backup-1.key`, made at first start) seals each repository's key-encryption key,
made with the repository, which seals each workload's data key. Each seal is XChaCha20-Poly1305 with
what it is for bound in, so a sealed key copied to another repository or workload does not open. The
database holds only sealed keys, so a copy of it alone opens no backup; the backup key is what a
[control plane restore](control-plane-restore.md) must keep, unrotated, or every backup made under
it is lost.

## Backup targets

**A backup target is a provider that holds the target's credential and never hands it out.** Every
store a repository names is reached through a `backup_target` provider, an extension like any other,
so the store's credential lives in that extension's configuration alone. No object byte passes
through its calls:

| Call | Asked by | Answers |
|---|---|---|
| `GrantAccess{prefix, mode, ttl}` | the control plane, per job | access to that prefix alone for 15 minutes to 12 hours, never longer than asked: `write` puts, gets and lists; `read` gets and lists; neither deletes |
| `ListObjects{prefix, after, limit}` | the control plane, pruning | up to a thousand keys beneath the prefix, in order |
| `DeleteObjects{keys}` | the control plane, pruning | how many of up to a thousand it deleted |

Access is S3-shaped (endpoint, region, bucket, the prefix's place in it, a temporary key, secret and
session token, and path or host addressing), because the S3 API is what every store either speaks or
fronts, and the node then needs one client. Fifteen minutes is the floor because a store's token
service issues nothing shorter, and a provider asked for less would grant more than asked, which a
prune's sweep, relying on no access outliving what was recorded, cannot afford. A provider that can
neither scope a store's access nor serve it answers `backup.unscoped`. The control plane checks
every request (a relative prefix ending in `/`, a deletion only beneath the prefix it prunes) and
every answer: access whose place is not the prefix asked, or a listing straying outside it, is
`provider.malformed`, counted by the breaker, and never reaches a node.

**The secret is in the database for as long as it takes to be read.** A call's answer travels back
through the call queue, a Postgres row, so the temporary secret is written there; the caller removes
it in the transaction after reading it, and the sweep removes it from any answer whose caller died a
minute later. A database reader or backup within that window holds a credential that can write one
prefix until it expires, which is less than the database holds already under operator custody. A
scoped S3 credential can overwrite a key; chunks are content-addressed, so an overwrite writes the
same bytes, and a record's key names its backup, which no other job writes. Where a store supports
conditional writes, a provider should refuse overwrites.

**The S3 target**, `korpis-target-s3`, is built on the extension SDK alone, as a third party's would
be. Its configuration, from its workload's environment, is the store's address, bucket, its place in
the bucket and its own credential. For each job it calls the store's `AssumeRole`, signed with its
own credential, with a session policy allowing the mode's actions beneath one prefix for exactly the
time asked. Its credential must be allowed to assume the role and do what the policy allows, since
the store grants what both allow; on AWS that role is `KORPIS_S3_ROLE_ARN`, while MinIO assumes the
caller. It cannot serve a store without a token service, such as B2's or GCS's S3 interfaces.

## Targets that serve their store

A provider in front of a store that cannot scope access (a disk, SFTP, a self-hosted store without a
token service) answers `GrantAccess` with a credential it minted for the job, scoped and lasting as
asked, and serves the S3 subset a job uses (get, put, list, never delete) on an endpoint of its own
workload over TLS. It does not know the address it is reached at, so its answer names the endpoint,
`served_by`, and the control plane fills in the address. **What evaluates this:** the control plane,
at the grant, from the provider workload's placed endpoints: the endpoint's node, the port that node
holds for it, and the node's advertised address. An endpoint whose exposure holds no port is
`backup.unreachable`. Its certificate is its own, made at start, and the answer carries it; the
client trusts that certificate and no other, which is sound because the answer reaches it only
through the provider's authenticated stream and the control plane. The cost: every byte of a job
passes through one extension, and the store is published on a port of its node, guarded by signed
requests alone, as an object store on the internet is.

**The filesystem target**, `korpis-target-fs`, is the first such provider: the provider's own volume
as a backup target, for an installation of one machine with no object store. Its job credentials
live in its memory alone, so a restarted provider has issued none and no access outlives what the
control plane recorded. Its environment sets where objects live (`/data`), its port (9000), its
endpoint (`store`) and the name it provides as (`fs`); its certificate is an ECDSA P-256 key's. A
repository on it does not survive the machine's loss, and is labelled not off-site.

## Format

The format is `korpis.backup.v1`, in `korpis-backup`. Beneath a workload's prefix are three kinds of
object, each compressed with zstd, then encrypted with XChaCha20-Poly1305 under a fresh nonce with
its own name as associated data, so an object copied to another name, or offered as another kind,
fails to open: a chunk (`chunks/<id>`), a run of a file's bytes; a tree (`trees/<id>`), one
directory, entries sorted by name; and a backup (`backups/<id>`), one record naming its root tree.
Every key is derived from the workload's data key with HKDF-SHA256: one for chunk and one for tree
identifiers (keyed BLAKE3), one for a content digest, one encryption key per kind.

**One tree per directory, content-addressed, is the manifest.** An unchanged directory encodes to
the same tree, stored once across every backup containing it, and a single-path restore fetches the
record, the trees on the path and that path's chunks. A file is read only where the filesystem says
it has data (`SEEK_DATA`, `SEEK_HOLE`), each run cut by FastCDC at 512 KiB to 8 MiB, averaging 1
MiB, so an insertion moves only its neighbouring chunks and a sparse file restores sparse. The
record is written last, so a backup whose record exists is whole and a failed one leaves only
objects the next reuses.

**The next backup reads only what may have changed.** A file whose size, modification time, change
time and inode match the parent's entry takes that entry's chunks unread. The change time cannot be
set by the file's owner, so a workload cannot make a changed file look unchanged.

**The walk cannot be steered out of the volume.** Every directory is opened relative to its parent
with `O_NOFOLLOW` and every inode examined through the descriptor opened, so a directory swapped for
a link during a live read is recorded as the link, never followed, and the walk never leaves the
volume's filesystem.

**A restore creates only what it was told, where it was told.** Every name is checked first (not
empty, `.` or `..`, no `/` or NUL, each once), every file created exclusively relative to a
directory the restore created, a whole volume restored only into an empty directory, one path only
beside what is there. Ownership, mode, extended attributes (POSIX ACLs among them), then times are
applied in that order, since each undoes part of the one before otherwise. Hard links are links
again, each name carrying the data. Verification walks what was restored and compares kind, mode,
owner, modification time, size, content by keyed digest (holes as zeros), link target, device
number, extended attributes and link groups; data is compared by digest because a filesystem may lay
out a sparse file's runs differently.

**The node's client** presigns each request for five minutes with the job's credential, sends it
over HTTPS verified against the node's system roots (plain HTTP only to a store on the node itself)
or the one certificate a serving provider's access carries, and retries only failures that pass (the
network, a 5xx, a 429). It writes and reads only beneath the granted prefix and believes a listing
only as far as it stays there.

What it does not do: restore a socket, since nothing listens on it afterwards; record a symbolic
link's extended attributes, since Linux refuses `user.` attributes on links; or delete anything,
pruning being the control plane's. Anything holding the data key can write objects that open, which
is the node running the workload, which could already read the live data.
