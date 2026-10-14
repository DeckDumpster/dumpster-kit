-- The fixture catalog the lake crates' own tests run against: two holdings tables with
-- the outbox triggers a site writes for each of its sources. It is NOT any site's
-- schema; it exists so the shipper, the zone reader and the erasure path are exercised
-- without a site. The outbox/cursor/gap tables come from dumpster_kit_ship::outbox::SCHEMA_SQL
-- (the kit owns those), and the triggers below are the part a site writes for its own tables.

CREATE TABLE IF NOT EXISTS collection (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    printing_id TEXT NOT NULL,
    condition   TEXT NOT NULL DEFAULT 'Near Mint',
    acquired_at TEXT NOT NULL,
    source      TEXT NOT NULL,
    notes       TEXT,
    purchase_price REAL,
    sale_price  REAL,
    graded      INTEGER NOT NULL DEFAULT 0,
    status      TEXT NOT NULL DEFAULT 'owned'
);

CREATE TABLE IF NOT EXISTS sealed_collection (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    product_id INTEGER NOT NULL,
    quantity   INTEGER NOT NULL DEFAULT 1,
    status     TEXT NOT NULL DEFAULT 'owned',
    added_at   TEXT NOT NULL
);

CREATE TRIGGER IF NOT EXISTS collection_outbox_insert
AFTER INSERT ON collection
BEGIN
    INSERT INTO ownership_outbox (occurred_at, source_table, op, row_id, payload)
    VALUES (
        strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'collection', 'insert', NEW.id,
        json_object(
            'id', NEW.id,
            'printing_id', NEW.printing_id,
            'condition', NEW.condition,
            'acquired_at', NEW.acquired_at,
            'source', NEW.source,
            'notes', NEW.notes,
            'purchase_price', NEW.purchase_price,
            'sale_price', NEW.sale_price,
            'graded', NEW.graded,
            'status', NEW.status
        )
    );
END;

CREATE TRIGGER IF NOT EXISTS collection_outbox_update
AFTER UPDATE ON collection
BEGIN
    INSERT INTO ownership_outbox (occurred_at, source_table, op, row_id, payload)
    VALUES (
        strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'collection', 'update', NEW.id,
        json_object(
            'id', NEW.id,
            'printing_id', NEW.printing_id,
            'condition', NEW.condition,
            'acquired_at', NEW.acquired_at,
            'source', NEW.source,
            'notes', NEW.notes,
            'purchase_price', NEW.purchase_price,
            'sale_price', NEW.sale_price,
            'graded', NEW.graded,
            'status', NEW.status
        )
    );
END;

CREATE TRIGGER IF NOT EXISTS collection_outbox_delete
AFTER DELETE ON collection
BEGIN
    INSERT INTO ownership_outbox (occurred_at, source_table, op, row_id, payload)
    VALUES (
        strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'collection', 'delete', OLD.id,
        json_object(
            'id', OLD.id,
            'printing_id', OLD.printing_id,
            'condition', OLD.condition,
            'acquired_at', OLD.acquired_at,
            'source', OLD.source,
            'notes', OLD.notes,
            'purchase_price', OLD.purchase_price,
            'sale_price', OLD.sale_price,
            'graded', OLD.graded,
            'status', OLD.status
        )
    );
END;

CREATE TRIGGER IF NOT EXISTS sealed_collection_outbox_insert
AFTER INSERT ON sealed_collection
BEGIN
    INSERT INTO ownership_outbox (occurred_at, source_table, op, row_id, payload)
    VALUES (
        strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'sealed_collection', 'insert', NEW.id,
        json_object(
            'id', NEW.id,
            'product_id', NEW.product_id,
            'quantity', NEW.quantity,
            'status', NEW.status,
            'added_at', NEW.added_at
        )
    );
END;

CREATE TRIGGER IF NOT EXISTS sealed_collection_outbox_update
AFTER UPDATE ON sealed_collection
BEGIN
    INSERT INTO ownership_outbox (occurred_at, source_table, op, row_id, payload)
    VALUES (
        strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'sealed_collection', 'update', NEW.id,
        json_object(
            'id', NEW.id,
            'product_id', NEW.product_id,
            'quantity', NEW.quantity,
            'status', NEW.status,
            'added_at', NEW.added_at
        )
    );
END;

CREATE TRIGGER IF NOT EXISTS sealed_collection_outbox_delete
AFTER DELETE ON sealed_collection
BEGIN
    INSERT INTO ownership_outbox (occurred_at, source_table, op, row_id, payload)
    VALUES (
        strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'sealed_collection', 'delete', OLD.id,
        json_object(
            'id', OLD.id,
            'product_id', OLD.product_id,
            'quantity', OLD.quantity,
            'status', OLD.status,
            'added_at', OLD.added_at
        )
    );
END;
