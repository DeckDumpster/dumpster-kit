use rusqlite::Connection;

use crate::error::Result;

/// The kit's DDL. `prices` is append-only: triggers refuse UPDATE and DELETE. `finish` is
/// the empty string for a game that prices by printing id.
pub const KIT_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS printing_source_id (
    source      TEXT NOT NULL,
    source_id   TEXT NOT NULL,
    sub_type    TEXT NOT NULL DEFAULT '',
    printing_id INTEGER NOT NULL,
    PRIMARY KEY (source, source_id, sub_type)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS printing_source_id_printing ON printing_source_id (printing_id);

CREATE TABLE IF NOT EXISTS prices (
    printing_id INTEGER NOT NULL,
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
CREATE TRIGGER IF NOT EXISTS prices_no_update BEFORE UPDATE ON prices
BEGIN SELECT RAISE(ABORT, 'prices is append-only'); END;
CREATE TRIGGER IF NOT EXISTS prices_no_delete BEFORE DELETE ON prices
BEGIN SELECT RAISE(ABORT, 'prices is append-only'); END;
CREATE VIEW IF NOT EXISTS latest_prices AS
    SELECT p.* FROM prices p
    WHERE p.observed_on = (SELECT MAX(q.observed_on) FROM prices q
        WHERE q.printing_id = p.printing_id AND q.finish = p.finish AND q.source = p.source);

CREATE TABLE IF NOT EXISTS tcgcsv_categories (
    category_id  INTEGER PRIMARY KEY,
    name         TEXT NOT NULL,
    display_name TEXT,
    modified_on  TEXT,
    sealed_label TEXT,
    popularity   INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS sealed_products (
    sealed_id INTEGER PRIMARY KEY,
    source    TEXT NOT NULL,
    source_id TEXT NOT NULL,
    group_id  INTEGER,
    name      TEXT NOT NULL,
    UNIQUE (source, source_id)
);

CREATE TABLE IF NOT EXISTS catalog_alarms (
    alarm_id  INTEGER PRIMARY KEY,
    kind      TEXT NOT NULL,
    game      TEXT NOT NULL,
    subject   TEXT NOT NULL,
    detail    TEXT NOT NULL DEFAULT '',
    UNIQUE (kind, game, subject)
);
";

pub fn install(conn: &Connection) -> Result<()> {
    conn.execute_batch(KIT_SCHEMA)?;
    Ok(())
}
