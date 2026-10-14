-- The tables the kit owns beside a site's holdings tables. Idempotent.
--
-- No tenant column: one collection is one database file, so the file IS the tenant, and a
-- `database_id` here would be a second, staler copy of what the registry says.
--
-- `source_table` carries no CHECK for the same reason it exists: this schema has no migration
-- mechanism, so a constraint added later needs an ALTER nothing here can express, whereas
-- another source only needs three more triggers.

CREATE TABLE IF NOT EXISTS ownership_outbox (
    seq           INTEGER PRIMARY KEY AUTOINCREMENT,
    occurred_at   TEXT NOT NULL,             -- UTC ISO-8601, millisecond
    source_table  TEXT NOT NULL,
    op            TEXT NOT NULL CHECK (op IN ('insert', 'update', 'delete')),
    row_id        INTEGER NOT NULL,          -- <source_table>.id; unique only within a source
    payload       TEXT NOT NULL,             -- JSON object: the whole row (the pre-image for delete)
    source        TEXT NOT NULL DEFAULT 'trigger'
        CHECK (source IN ('trigger', 'backfill', 'redrive'))
);

CREATE INDEX IF NOT EXISTS idx_ownership_outbox_row
    ON ownership_outbox(source_table, row_id, occurred_at);

-- One row, ever: the highest seq known to be in the tenant zone. Written AFTER the object
-- lands, never before, which is what makes delivery at-least-once rather than at-most-once.
CREATE TABLE IF NOT EXISTS ownership_outbox_cursor (
    id            INTEGER PRIMARY KEY CHECK (id = 1),
    shipped_thru  INTEGER NOT NULL,
    shipped_at    TEXT NOT NULL
);

-- The durable half of gap detection. A missing seq means an event was LOST, and it is
-- recorded here BEFORE the cursor advances past it.
CREATE TABLE IF NOT EXISTS ownership_outbox_gap (
    from_seq      INTEGER NOT NULL,
    to_seq        INTEGER NOT NULL,
    detected_at   TEXT NOT NULL,
    PRIMARY KEY (from_seq, to_seq),
    CHECK (to_seq >= from_seq)
);

-- The provenance of the zone read-back (`zone::materialize`): which parts, how far through the
-- outbox, how many rows each source produced, and when. The staging tables themselves are NOT
-- declared anywhere: they are created from each source table's own `pragma_table_info`,
-- because a hand-written mirror would be a third place the shape is written down. Being
-- created rather than declared also means they carry no triggers, so materialising the zone
-- cannot emit outbox events. All of these are transport state: a site keeps them out of its
-- portable export.
CREATE TABLE IF NOT EXISTS zone_holdings_run (
    dataset       TEXT PRIMARY KEY,          -- 'holdings' — one row per dataset read
    parts         INTEGER NOT NULL,          -- objects read out of the zone
    events        INTEGER NOT NULL,          -- outbox events in them
    max_seq       INTEGER NOT NULL,          -- how far through the outbox the zone has got
    partitions    TEXT NOT NULL,             -- the as_of= dates read, comma-separated
    rows          INTEGER NOT NULL,          -- holdings the reduction produced, all sources
    per_source    TEXT NOT NULL,             -- JSON object: source_table -> rows produced
    read_at       TEXT NOT NULL              -- UTC ISO-8601
);
