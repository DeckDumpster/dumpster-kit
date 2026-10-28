/// Bump when the kit's own DDL changes: every catalog file is then rebuilt from raw landing.
pub const KIT_SCHEMA_VERSION: u32 = 1;

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
";
