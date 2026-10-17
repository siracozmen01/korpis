-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The epoch issue-rate cap, enforced (§8 of docs/03-state.md). issue_rate_per_sec was recorded
-- and nothing held issuance to it, so the bound a restore computes, the watermark plus the rate
-- times the backup's age, had no ceiling behind it. Issuance now spends from a bucket that holds
-- at most a second's worth and refills at the rate: in any interval of t seconds at most
-- rate x (t + 1) epochs are issued, which is the bound the restore advances past.

ALTER TABLE epoch_state
    ADD COLUMN bucket_tokens double precision NOT NULL DEFAULT 0 CHECK (bucket_tokens >= 0),
    ADD COLUMN bucket_at timestamptz NOT NULL DEFAULT now();
UPDATE epoch_state SET bucket_tokens = issue_rate_per_sec;
