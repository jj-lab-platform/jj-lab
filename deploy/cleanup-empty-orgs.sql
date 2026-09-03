-- One-shot cleanup (post-upgrade): drop orgs that no longer hold any repo.
-- Before this jjlab auto-cascaded org removal on the last repo delete, so any
-- surviving org rows here are legacy artifacts from before orgs became
-- first-class resources. Run once by hand, NOT at boot (idempotent but must
-- not erase newer explicitly-created empty orgs on every restart).
--
-- Usage:
--   sqlite3 "$JJLAB_DB" < deploy/cleanup-empty-orgs.sql
--   (JJLAB_DB defaults to /data/data.db)
DELETE FROM orgs
WHERE id NOT IN (SELECT DISTINCT org_id FROM repos);
