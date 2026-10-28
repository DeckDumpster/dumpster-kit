/// Bump when the kit's own DDL changes: every catalog file is then rebuilt from raw landing.
pub const KIT_SCHEMA_VERSION: u32 = 2;

pub const KIT_DDL: &str = "
CREATE TABLE catalog_schema (
    game            TEXT NOT NULL PRIMARY KEY,
    schema_version  INTEGER NOT NULL,
    ddl_hash        TEXT NOT NULL,
    kit_version     INTEGER NOT NULL
) STRICT;

CREATE TABLE applied_payloads (
    game          TEXT NOT NULL,
    source        TEXT NOT NULL,
    hash          TEXT NOT NULL,
    day           TEXT NOT NULL,
    applied_at    TEXT NOT NULL,
    rows_changed  INTEGER NOT NULL,
    PRIMARY KEY (game, source, hash)
) STRICT;

CREATE TABLE sync_state (
    game             TEXT NOT NULL,
    source           TEXT NOT NULL,
    last_hash        TEXT,
    last_day         TEXT,
    last_attempt_at  TEXT,
    last_success_at  TEXT,
    last_error       TEXT,
    PRIMARY KEY (game, source)
) STRICT;

CREATE TABLE alarms (
    id          INTEGER PRIMARY KEY,
    game        TEXT NOT NULL,
    kind        TEXT NOT NULL,
    subject     TEXT NOT NULL,
    detail      TEXT NOT NULL,
    raised_at   TEXT NOT NULL,
    cleared_at  TEXT
) STRICT;
CREATE UNIQUE INDEX alarms_one_open ON alarms (game, kind, subject) WHERE cleared_at IS NULL;

CREATE TABLE printing_source_id (
    source      TEXT NOT NULL,
    source_id   TEXT NOT NULL,
    sub_type    TEXT NOT NULL DEFAULT '',
    printing_id TEXT NOT NULL,
    PRIMARY KEY (source, source_id, sub_type)
) WITHOUT ROWID;
CREATE INDEX printing_source_id_printing ON printing_source_id (printing_id);

CREATE TABLE prices (
    printing_id TEXT NOT NULL,
    finish      TEXT NOT NULL DEFAULT '',
    source      TEXT NOT NULL,
    observed_on TEXT NOT NULL,
    low         REAL,
    mid         REAL,
    high        REAL,
    market      REAL,
    direct_low  REAL,
    PRIMARY KEY (printing_id, finish, source, observed_on)
) WITHOUT ROWID;
CREATE TRIGGER prices_no_update BEFORE UPDATE ON prices
BEGIN SELECT RAISE(ABORT, 'prices is append-only'); END;
CREATE TRIGGER prices_no_delete BEFORE DELETE ON prices
BEGIN SELECT RAISE(ABORT, 'prices is append-only'); END;
CREATE VIEW latest_prices AS
    SELECT p.* FROM prices p
    WHERE p.observed_on = (SELECT MAX(q.observed_on) FROM prices q
        WHERE q.printing_id = p.printing_id AND q.finish = p.finish AND q.source = p.source);

CREATE TABLE tcgcsv_categories (
    category_id  INTEGER PRIMARY KEY,
    name         TEXT NOT NULL,
    display_name TEXT,
    modified_on  TEXT,
    sealed_label TEXT,
    popularity   INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE sealed_products (
    sealed_id TEXT NOT NULL PRIMARY KEY,
    source    TEXT NOT NULL,
    source_id TEXT NOT NULL,
    group_id  INTEGER,
    name      TEXT NOT NULL,
    UNIQUE (source, source_id)
);
";
