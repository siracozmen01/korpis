-- Whether a live repository other than $4, of organization $1 and provider $2, has a place one
-- prune of which would reach into a place of a repository at prefix $3: the same prefix, or one
-- that starts with the other's followed by `workloads/` (Finding 32 of docs/23-walkthroughs.md).
-- Within an organization a provider name resolves to one installation; across organizations a
-- workload is only ever backed up into its own organization's repositories, under its own
-- identifier, so no prefix chosen in one can reach another's backups.
SELECT EXISTS (
    SELECT 1 FROM repository r
     WHERE r.deleted_at IS NULL
       AND r.id <> $4
       AND r.organization_id = $1
       AND r.provider_name = $2
       AND (r.prefix = $3
            OR starts_with($3, r.prefix || 'workloads/')
            OR starts_with(r.prefix, $3 || 'workloads/'))
) AS "overlaps!"
