# Backing up and restoring the control plane

How to keep what a control plane needs to be restored, and how to restore it. Tenant data is backed
up by workloads' own [backups](../internals/backup.md). Why each step exists is on [control plane
restore](../internals/control-plane-restore.md).

## What must be kept

| Kept | Why |
|---|---|
| the PostgreSQL database | every intent, grant, effect and placement |
| the grant signing key | tokens verify against it; rotated on restore |
| the assignment-signing keyring | verify-only keys that let a restore recover assignments the store lost |
| the node identity CA | agents pin it; not rotated, so nodes reconnect without re-enrolling |
| the backup key | seals every operator-custody repository's key; lost, no tenant backup under it can be read |
| the TLS key | seals every ingress certificate's key; lost, `acme` certificates are reissued and provided ones uploaded again |
| the factor key | seals every TOTP secret; lost, accounts sign in another way and enrol TOTP again |
| the ACME account key | lost, a new account is made at the next order |
| the epoch watermark and issue-rate cap | the restore's fence is computed from them |

**The backup lives outside the system it protects.** Writing the control plane's backup into a
Korpis repository is circular: the repository's key reference lives in the database being restored.
So the installer refuses an installation without a backup target of its own, a directory on another
device than the data directory, and its key is one you hold, printed once at install.

## Taking the backup

`korpis-server backup` writes everything in the table but the database as one file in the target:
every file under the data directory's `keys` and `tls`, and the epoch watermark with its rate cap.
The file is sealed with XChaCha20-Poly1305 under your key, written beside itself and renamed into
place, read back and opened before the command returns; the newest 30 are kept (`--keep`). The
installer writes a unit and timer running it daily as the control plane's own user, never root, so
nothing a compromised control plane could write into its data directory steers what root reads.

The database is PostgreSQL's own to back up, with continuous WAL archiving. The command checks that
`archive_mode` is on and the last archiving attempt did not fail, and fails after writing the keys
when the database is not being archived, so the gap shows in the unit's state rather than in a
disaster. Korpis does not configure archiving, and cannot check a base backup exists, which
PostgreSQL does not record.

## Restoring

1. Stop every control plane replica. A replica left running would issue epochs before the fence.
2. Restore the database with PostgreSQL's own tools to the point you choose.
3. Run, as the data directory's owner:

   ```
   korpis-server restore --from <backup file> --key-file <key>
   ```

   It puts the keys and certificates back: a missing file is written, an identical one kept, and a
   different one stops the restore before anything is written, since only you know which is right. A
   machine restored from nothing is given the backup's directory as its target. It then fences
   epochs from the file's watermark and rate, rotates the token signing key, holds every relay the
   backup knew, and writes the gap record. Each step is safe to run again, so a restore stopped part
   way is finished by running it again.
4. Start the control plane with its agents running. For the settling window (`--settle-secs`, ten
   minutes by default) every request needing a caller is answered `UNAVAILABLE` while agents present
   the epochs and assignments they hold; what the backup lacked is recovered from them with its
   identifiers. Sign-in and `ServerInfo` are served.
5. Readmit each relay you trust with `NodeService.ReadmitRelay`, or remove it. A held relay is named
   to no node and no client; until readmitted, sessions open only through the control plane's own
   relay role. A relay that joined after the backup is refused and must join again.

Every session re-authenticates after the key rotation, bounded by token lifetimes. What is lost is
stated: declarations made after the backup and never approved reached no node, and control-plane
effects in that interval with no node counterpart; the gap record names the interval. WAL archiving
keeps it short.

The restore does not check that the control plane is stopped, or that the database is the one the
file was taken with; both are yours to know.
