# Files

How a workload's owner reaches its files: the panel's file manager, `korpis files`, and SFTP all
open one kind of session, served on the node by a confined worker that sees the workload's volumes
and nothing else.

## Contents

- [The session](#the-session)
- [What a session can do](#what-a-session-can-do)
- [The worker](#the-worker)
- [What it records](#what-it-records)
- [SFTP](#sftp)
- [What it does not do](#what-it-does-not-do)

## The session

A file session is a data-path session (see [relays](relays.md)): opened at a relay with a capability
token for `workload.file.read`, or `workload.file.write`, which implies it, on one workload, carried
to its node, and served there by a filesystem worker the helper starts for the session. Its messages
are `files.proto`: one `FileRequest` per data frame up, one `FileResponse` down answering it by
number, and a refusal carried as a code (`not_found`, `exists`, `full` for `EDQUOT`, `outside`,
`refused`) that ends nothing.

The worker sees the workload's volumes and nothing else, so a session names only paths within them
and a symbolic link pointing outside resolves to nothing. It runs as the workload's root inside the
workload's user namespace, so what it writes is the workload's and counts against the volume's quota
in the write path: a full volume refuses a write with `EDQUOT`, reported as full. A volume holds
files of several of the workload's users and a session serves all its volumes at once, so no single
owner exists to run as. Instead the worker holds only `CAP_CHOWN`, `CAP_FOWNER` and
`CAP_DAC_OVERRIDE` in a session that writes and `CAP_DAC_READ_SEARCH` in one that reads, each over
the workload's ids alone, and gives what it makes to the owner of the directory it is made in, so a
server running as uid 1000 owns the plugin uploaded into its directory.

## What a session can do

List a directory, a page at a time however large it is; read a file's metadata; read from an offset;
write at an offset, creating or truncating first if asked; make a directory; rename within the
volumes, never replacing what is there; remove a file, or a directory and what is in it, at most 128
levels down. Reads and writes move at most a mebibyte, so a large file streams and never sits whole
in the worker, the relay or the browser, and serve only regular files, never waiting on a FIFO the
tenant made. A volume the workload mounts read-only (`mode: ro`) is never written.

**An upload resumes.** A client that lost its session reads the size of what it wrote and continues
from there, so a 40 GB world survives a dropped connection, and an upload never finished is a
partial file its owner can see and remove, not hidden state. That holds only if the file never holds
a chunk without every chunk before it, and a client sending chunks ahead of their answers could lose
one and write the next past it, leaving a hole a resume never fills. So a write may ask to land only
where the file ends (`at_end`), refused with `MOVED` and nothing written otherwise; the panel and
the CLI ask it of every chunk, so a lost chunk leaves every later one refused and the size a resume
reads is what was written, in order.

## The worker

The worker is `korpis-fsworker`. The helper's `OpenFiles` checks the workload's id range against its
ledger and each volume path as an install's, starts the worker with one end of a socket pair and
answers with the other, so a session's bytes never pass through the helper. The worker builds an
install worker's root (see [recipes](recipes.md)): its own mount namespace, the volumes bind-mounted
at their declared paths under an empty root, no network. It confines itself with Landlock to its
volumes (their reading alone in a session that reads), drops every capability but those above, and
serves one length-prefixed `FileRequest` after another until the socket closes.

Every path is resolved from the root's descriptor with `openat2` and `RESOLVE_BENEATH`,
`RESOLVE_NO_SYMLINKS`, `RESOLVE_NO_MAGICLINKS` and `RESOLVE_NO_XDEV`, so a planted link, a `..`, or
a step onto another mount reaches nothing, and a link is listed and never followed. Each path is
served by the volume mounted at its longest prefix, beneath that volume's own descriptor, so a
volume inside another is reached as itself; the root they are mounted in can be listed and nothing
in it changed.

The node's handler names the volumes from the assignment it holds, journals the session's opening
before asking for a worker, and carries frames both ways unchanged with up to sixteen requests
ahead. For a `microvm` the same worker runs inside the guest (see [guests](guests.md#file-access)).

## What it records

Opening and ending a session are journaled by the node, as console input is (see
[streams](streams.md)), as acts of the session's subject under the grant the token was minted from,
and the control plane believes them only if that grant permitted the action then. Every change is
recorded with the end, as one effect: `files`, `bytes`, `directories`, and the `removed` and
`renamed` paths up to a thousand, then their count. Reads are not recorded one by one, but every
opening is, on the workload, so a session under a grant scoped above the workload's organization, a
host reading a customer's files, is on the tenant's own record as operator access. A session ending
because its worker or client failed is recorded as failed; a node that dies during one leaves its
opening without an end, and the missing end is the record.

## SFTP

An SFTP client reaches the same session through a relay that serves it, with an SSH key (see
[authentication](authentication.md)); the relay is the session's client, speaking SFTP version 3,
which every client speaks. Directory reads list page by page; `STAT` and `LSTAT` read metadata, a
link shown as a link; `OPEN`, `READ` and `WRITE` work at the client's offsets, creating or
truncating as its flags ask; `MKDIR`; `RENAME` without replacing, which is what version 3 means;
`REMOVE`; and `RMDIR` only once empty, as the protocol says. `REALPATH` answers paths beneath `/`,
the volumes at their declared paths. A read-only token refuses every change as permission denied.
What the session cannot do is refused as unsupported rather than faked: `SETSTAT` and `FSETSTAT`,
since it sets no permission or time; `SYMLINK`; and `READLINK`, since a link is never followed.

The relay forwards reads and writes as they arrive, up to the sixteen the node holds unanswered, and
answers any other request once those before it are answered, so it sees what they did. `OPEN`
creates or truncates through an empty write; otherwise it looks first, so a handle names a file that
is there. `EXCLUDE` is that look, which a second writer can race, since the session has no
create-if-absent verb; `APPEND` writes where a look says the file ends. `REMOVE` and `RMDIR` look
first so neither removes the other's kind. A listing carries the line `ls -l` prints, its
permissions saying what a session lets anyone do.

An SFTP client writes several offsets in flight, so `at_end` is not asked of it: a connection lost
mid-upload can leave a hole the client's resume, continuing from the file's size, does not fill.
That is how every SFTP server behaves; a person who needs the guarantee uploads with the panel or
`korpis files sync`.

## What it does not do

It does not compress or extract archives, set permissions, or follow a link out of the volumes: a
recipe's install does what a recipe needs, and the rest is for the workload's own tools. It carries
no digest, so `korpis files sync` tells a changed file by size and time and sends it whole, and a
resumed upload cannot check the bytes already there; a verb answering a range's SHA-256 would fix
both at the cost of the worker reading that range. Each session costs a worker process, started in a
few milliseconds, and its writes are recorded when it ends, so an auditor sees a long session's
changes when it closes.
