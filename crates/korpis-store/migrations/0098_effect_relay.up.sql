-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The client that relayed what a person presented, on every effect of a call they made through
-- it (§5.2 of docs/08-identity.md): the actor is the person, the grant is theirs or a set's,
-- their provider is in their key, and this names the client, so the effect names everyone.

ALTER TABLE effect ADD COLUMN relay text;
