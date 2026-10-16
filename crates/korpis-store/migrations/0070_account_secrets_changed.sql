-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- When an account's guessable secrets, its password and its TOTP factor, last changed (§9.6 of
-- docs/08-identity.md, Finding 73 of docs/23-walkthroughs.md). A device cookie issued before it
-- is not honoured, so a browser that signed in with a password since replaced keeps no budget
-- of guesses at its successor.

ALTER TABLE account ADD COLUMN secrets_changed_at timestamptz;
