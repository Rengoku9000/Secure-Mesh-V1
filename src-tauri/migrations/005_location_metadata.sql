-- Location provenance for incidents.
--
-- WHY
--
-- Coordinates alone do not say how much to trust them. A satellite fix good to
-- five metres and a Wi-Fi estimate good to fifty kilometres are the same two
-- numbers, and a responder deciding whether to drive somewhere needs to tell
-- them apart. A receiving node had no way to: accuracy, source and capture time
-- existed only on the machine that took the reading.
--
-- These three columns make that provenance part of the authoritative record, so
-- it replicates inside the signed incident event with everything else.
--
-- EXISTING DATA
--
-- Every column is nullable and no default is applied, so incidents recorded
-- before this migration keep exactly what they had. They will read as
-- accuracy unknown, source UNKNOWN, capture time unknown — which is true.
--
-- `location_captured_at` is deliberately NOT backfilled from `created_at`.
-- They answer different questions: `created_at` is when the operator filed the
-- record, `location_captured_at` is when the position was measured. They are
-- usually seconds apart and occasionally not, and inventing the second from the
-- first would put a fabricated measurement time into a signed record.
--
-- Nothing here fabricates historical accuracy. An old incident that reads
-- "accuracy unknown" is reporting the truth: nobody recorded it.

ALTER TABLE incidents ADD COLUMN accuracy_meters REAL;
ALTER TABLE incidents ADD COLUMN location_source TEXT;
ALTER TABLE incidents ADD COLUMN location_captured_at TEXT;

-- SQLite cannot add a CHECK constraint to an existing table, so these
-- invariants are enforced in `domain::incident::Location` on the way in. The
-- rules are: accuracy is finite and non-negative, source is one of
-- GNSS / WIRELESS / UNKNOWN, and none of the three may be present without
-- coordinates to describe.
--
-- Recording that limitation here rather than leaving it implied: the database
-- checks coordinates but trusts the core for their provenance.
