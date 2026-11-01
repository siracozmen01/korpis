# Backup

How a volume is backed up, kept, pruned and restored. Who holds which key and credential, the backup
targets, and the format are on [backup keys and format](backup-format.md).

## Contents

- [The flow](#the-flow)
- [What the design gives](#what-the-design-gives)
- [Taking a backup](#taking-a-backup)
- [Repositories](#repositories)
- [Retention](#retention)
- [Pruning](#pruning)
- [Restore](#restore)
- [Block devices](#block-devices)
- [Recipe content by reference](#recipe-content-by-reference)
- [The job](#the-job)

## The flow

```
1. snapshot the volume                        instant, atomic, the workload never pauses
2. read from the snapshot                     one point in time, not a moving target
3. chunk at content-defined boundaries        FastCDC
4. hash, encrypt, upload only new chunks      deduplication before the network
5. write the record, a tree of references
6. release the snapshot
```

**The workload never stops.** The snapshot takes milliseconds on btrfs or ZFS, and everything after
reads a frozen view while the workload writes the live one. Pterodactyl's backup reads the live
directory, so an archive of a running game server is inconsistent by construction. On a class
without `snapshot`, Korpis reads the live tree and says so in the backup's record: what changed or
vanished under the walk is listed as skipped.

## What the design gives

| Property | Mechanism | Closes |
|---|---|---|
| incremental by default | unchanged chunks are already in the repository | pterodactyl/panel#5493 |
| no size limit | chunks are megabytes; multipart is irrelevant | #2599 |
| no local staging | chunks stream to the repository | #3846 |
| does not starve the node | a throttled cgroup | #2798 |
| deduplicated | content-addressed chunks within a workload's history; recipe content by reference | |
| encrypted before leaving the node | a per-workload data key | |
| single-file restore | the record is a tree; fetch only what is needed | |
| per-target policy | repositories are objects with their own settings | #4411 |

**Deduplication has a boundary, and it is a security boundary.** Every backup of a workload
deduplicates against that workload's earlier ones, so nightly backups of a 40 GB world cost the
day's changes. Across workloads chunks are not shared: content-addressed dedup leaks whether a file
already exists, which across tenants is a confirmation-of-file oracle, and each workload's chunk
identifiers are keyed by its own data key, so two workloads storing one file produce unrelated
identifiers. What fifty servers running one modpack share is the modpack: files byte-identical to
their recipe's content are recorded by reference to its digest and stored once per repository, and
recipe content is public, so there is nothing to confirm. The one-key-per-repository alternative
would have needed every node to hold a key decrypting every workload's backups.

Single-file restore falls out of the tree: restoring one configuration file from three weeks ago
fetches a few kilobytes, where Pterodactyl downloads and extracts the whole archive.

## Taking a backup

`korpis backup create vol_... --repository rep_...` proposes, under `backup.create` on the volume's
workload and `repository.read` on the repository, a plan of one `backup_volume` step, which changes
nothing the workload sees and applies at once. The snapshot and its release are inside that step,
because a step deleting a snapshot is irreversible and would make every backup wait for a person.
The repository must serve the workload's project; one backup of a workload into a repository is open
at a time (`backup.busy`), which is one writer per prefix; and the volume's last whole backup there
is the parent, whose unchanged files are not read again.

The backup is recorded pending with its plan and made whole only by the report of the node that ran
its job; one whose plan ended without that report reads as failed, with the reason. When the step
begins, the node holding the volume asks for the job's key and access naming the plan and step, and
the control plane answers only that node, only while the plan is open and the backup pending. A
backup's record is read under `workload.read`. Nothing backs up on a schedule: a nightly backup is a
`scheduled` workload whose run proposes one under its own grants (see [runs](runs.md)).

## Repositories

```
Repository
  id, name, scope        organization, or one of its projects
  target                 the organization's backup target provider, by name, and a prefix
  key_custody            operator | tenant
  retention              keep_last, keep_hourly, keep_daily, keep_weekly, keep_monthly,
                         keep_yearly, keep_failed, min_retention
  streams                its workloads' console streams are offloaded here
```

Making one takes `repository.write`, reading one `repository.read`. It names its provider by name,
so the provider need not be serving when it is made, and its place by a relative prefix of letters,
digits, `.`, `_` and `-` between slashes, since a prefix is written into a store's policy, where
`*`, `?` and `${...}` are patterns. Each workload's objects are beneath
`<prefix>workloads/<workload>/`, written only by that workload's jobs; offloaded streams beneath
`<prefix>streams/<workload>/`, beside the backups and never beneath them, so a prune never reaches
them; recipe content beneath `<prefix>content/`.

**Two repositories of an organization never share a place.** A second repository naming the same
provider at the same prefix, or one beginning with the other's followed by `workloads/` or
`streams/`, is refused (`repository.prefix_overlaps`): one workload backed up into both would write
one prefix under two keys, and each prune would take the other's objects for garbage. Within an
organization a provider name resolves to one installation, which makes the comparison meaningful.
Across organizations no check is made, since it would tell one tenant where another keeps its
backups, and a workload is only backed up into its own organization's repositories beneath its own
identifier. Two provider names pointed at one bucket and prefix are invisible to the control plane
and must be avoided in the providers' configuration.

**Widening a repository's scope is decided by the people inside it.** A reseller could put forty
customers in one repository for a cheaper bill, and with a shared key would hand each an oracle
against the rest: the party deciding is not the party bearing it. So a repository scoped to one
organization is that organization's decision; spanning organizations needs the authority of the
owner of every organization it spans, as [extensions](extensions.md) require for providers; and the
scope is visible to every tenant inside it. Per-workload keys mean a shared repository shares no
oracle, only storage and public recipe content, and the rule stays because where a tenant's data
lives is the tenant's question.

**Korpis runs no object store.** A single-machine operator uses the filesystem target on a separate
disk or a mounted remote filesystem, and a target on the same machine as the data it protects is
labelled not off-site wherever its backups are shown. Anything S3-compatible serves the rest.

## Retention

Each volume's backups are judged on their own. A whole backup is kept if it is one of the volume's
newest `keep_last`, or the newest of its hour, day, ISO week (Monday to Sunday, in the year of its
Thursday), month or year, among the newest that many periods that have a backup, all in UTC: periods
without one are not counted, so a server off for a week still keeps seven dailies. So any policy
that keeps something keeps each volume's newest whole backup, its next backup's parent. A policy
keeping nothing is refused. Whatever the policy, a backup younger than `min_retention` is kept, as
is one not finished, one an open plan restores, and one a reader was given access to that has not
ended; a failed backup's record is kept for `keep_failed`, so why it failed can be read. Every
decision names the rules keeping the backup.

`min_retention` is a floor no policy, operator or automation can prune below: the defence against a
misconfigured rule quietly destroying the history a ransomware incident would need.

## Pruning

Pruning while a backup runs can delete a chunk the running backup just decided not to upload because
it existed, which is how deduplicating backup systems lose data. The rule preventing it is simple
because the keys made it so: a workload's chunks live only under its own prefix, and the control
plane schedules every job touching a prefix, never two at once. A prune lists and deletes beneath
one workload's prefix at a time, never the repository's whole prefix.

`korpis backup prune <workload> --repository rep_...` proposes one `prune_backups` step under
`backup.prune`. It is irreversible, so never approved by default, and its detail names each backup
it forgets and why every other is kept; nothing is proposed while every backup is kept or being
read. The step is the control plane's: it forgets the named backups, except one being read by then,
and marks every object beneath the prefix that the remaining records do not reference, reading what
references what itself, with read access and the workload's key, from each record's root tree down.
A node is never asked: a host that could name what to delete could delete its own history, which a
backup exists to outlive.

The sweep is queued thirteen hours later, an hour longer than any access a job is given, and begins
only when no backup into the prefix is open and no write access to it is still live: every access a
job or reader is given is recorded before it is asked for, with its end, so a job whose plan ended
while it ran is still seen. It then holds the prefix, so a backup proposed meanwhile is refused,
reads the references again, deletes what is marked, still there and still unreferenced, and unmarks
the rest. A lock lost to a crash cannot race a writer into a hole. Recipe content beneath
`<prefix>content/` is never pruned and outlives every backup referring to it. Pruning runs with the
repository's own credential, which only its target's provider holds, through the provider's calls.

The cost: a prune reads every tree of every kept backup twice, through the control plane; trees are
small and shared, but a volume of millions of files is millions of small reads.

## Restore

| Mode | Does |
|---|---|
| into a new directory | a whole backup or one path, beside what the volume holds; loses nothing, applies at once |
| replace | the whole volume becomes what the backup held; waits for a person |
| clone | a new workload like the backup's, created holding it: how a restore is tested |
| point in time | any backup in the chain, not only the latest |

**Restoring into something new is the default.** Restoring over live data destroys the current
state, occasionally what someone wants and often a second disaster on the first, so the safe
operation is the prominent one.

`korpis backup restore bkp_... --into <dir>` restores a backup, or one `--path`, into a new
directory at the root of a volume, the backup's own unless `--volume` names another, refusing a
directory that exists. `--replace` restores into a staging place beside the volume while the
workload runs, then `stop`, `replace_volume`, `start`, so the workload is down only while the helper
exchanges the two in one rename. The staging place is its own subvolume or quota project carrying
the volume's limit, so the restore is not counted against the live volume; done again after a crash
the exchange changes nothing, since the staging place's identity, recorded when made, says whether
it happened. Replacing needs room for two copies, which nothing reserves: a restore running out
fails and leaves the volume as it was.

Restoring a workload's backup into it is `backup.restore` there; into another workload's volume
hands its contents to that workload's holders, so it takes `backup.read` on the first and
`workload.file.write` on the second. A declared volume may name `from_backup`, created holding a
whole backup before anything runs on it, under `backup.read` on the backup's workload, checked only
for volumes a declaration creates. `korpis backup clone` composes that.

**Browsing is a separate action from reading.** A backup's tree is every path, name and size as of
that snapshot: for a game server close to harmless, for a database, a mail spool or an uploads
directory most of the disclosure. A support engineer helping someone find a file should not
enumerate forty days of filenames. So `backup.browse` (names, kinds, sizes, times, no byte of any
file) and `backup.read` are separate, neither implies the other, and both are recorded as sensitive
reads before anything is read. `korpis backup ls` and `cat` read through the control plane with read
access to the prefix and the workload's key, fifteen minutes of it for a listing and a job's twelve
hours for a file; a file streams with its holes as zeros and is checked against the digest the
backup recorded, so a stream ending without an error is the file.

**Restores are verified.** A restore that completes without confirming the data is a hope. The
restored tree is walked as a backup would walk it and compared entry by entry, including links,
extended attributes, ACLs, holes and hard links, the omission that makes Pterodactyl's transfers
produce unbootable servers (pterodactyl/panel#5429).

## Block devices

A block device's backup is its image, and its record says which filesystem the image holds
(`image_filesystem`), `ext4` for every block device Korpis has, since the helper formats them so and
`korpis-guest` mounts nothing else. `korpis backup list` says it holds an image, not files. The host
never parses a filesystem a tenant's guest wrote, so reading inside it runs in a service guest (see
[guests](guests.md#service-guests)):

- **Browsing and reading** are a read-only file session of `backup.browse` or `backup.read` whose
  parameters name the backup. The node holds neither the repository's credentials nor which workload
  a backup is of, so it asks the control plane with the session's token. The control plane verifies
  the token again and the grant it was minted from as it stands now, so a revocation since is
  honoured; checks the backup is the workload's, whole, an image, of a volume that node holds;
  records the read before anything is read; and grants read access to the prefix. The helper
  restores the image into a reading place beside the volume (`.<volume>.read+<job>`), one per backup
  on the node however many sessions read it, and the driver serves it at `/backup`. A browse session
  ends at its first read of bytes. The last session's end discards the copy. The API's
  `BrowseBackup` and `ReadBackupFile` read such a backup as its one image, never inside it, since
  the control plane parses nothing of a tenant's.
- **A path restored into the volume** stages the image while the workload runs, stops it, has the
  driver copy the path inside a service guest holding both, and starts it again. The copy is the
  install job's: laid out as a restore of files lays it out, modes, owners within the workload's
  ids, times and holes kept, links never followed, a device, FIFO or socket refused by name, made in
  a fresh directory renamed into place last so a copy asked again finds its own work. The workload
  is down for the copy, and a file linked twice is copied twice.
- **A whole backup** restores into a volume a declaration just made (`from_backup`) or by a
  replacing restore, its staging place marked a block device's as its volume is.

## Recipe content by reference

A backup is given the workload's recipe content, the recipe's files and what its install fetched,
each by sha256 and size as the node's content store holds it, and a file byte-identical to one is
recorded by reference, storing nothing. Content the node cannot name when the backup starts is
backed up as any file. The repository keeps its own copy of referenced content beneath
`<prefix>content/`, once for every workload in it: a backup job is granted that prefix to write and
keeps there what the repository lacks, and a restore job reads there what the node's store lacks.
Content is kept in parts of at most 8 MiB with an index written last, so a copy whose index exists
is whole; unsealed, since it is public; and read back only if it hashes to its name. A node with
write access can replace a part, which makes that content fail its check and its restore refuse it:
an availability loss, never a wrong byte.

## The job

A backup or restore is a `korpis-backup-job` process the helper starts (`StartJob`) and then only
watches (`JobStatus`, `CancelJob`, `ForgetJob`). The supervisor names the volume, snapshot, backup
and any directory to make, all as identifiers; the helper turns them into paths, makes a cgroup for
the job beside the workloads' with a tenth of their CPU and I/O weight and ceilings on memory and
processes, and writes the job's spec, with the data key and the store's access, to the job's
standard input and nowhere else. The job's first act, while it is one thread, is to confine itself:
join that cgroup, bind Landlock to its directory (read-only for a backup), its result's directory,
the certificate and resolver files a connection reads, and connections to the store's port, and drop
every capability but its work's from every set, the bounding set included. What each confinement
held is in the result, as the kernel reported it. A job outlives a helper that exits: a restarted
helper finds a running job by the process its pid file names, if still the job binary, and reports
one that ended without a result as lost. An install is a job of the same kind (see
[recipes](recipes.md)).

Not verified end to end: ZFS, and a target other than S3 and the filesystem target. Not built: a
backup of a `vm`'s memory state with `runtime_snapshot`, which would let a restore resume rather
than boot, at the cost of the RAM size per backup.
