-- Registry database schema — the logical→physical map (pd-fci1).
--
-- One row per user: the handle the outside world names them by, and the
-- opaque database id that names their file on disk. Two separate facts,
-- joined by this table rather than by string equality.
--
-- Its own file at the data root: NOT a table in shared.sqlite (which every
-- tenant connection ATTACHes readable, so a roster there would be visible to
-- every tenant), and NOT under tenants/ (which would break "every *.sqlite
-- under tenants/ is a tenant", the statement the Litestream glob rests on).
--
-- Re-applied idempotently on every open, like the other two schemas.
--
-- Three rules live HERE rather than in the accessor, because the data model
-- for the application belongs in the schema:
--
--   1. `database_id` is the PRIMARY KEY. It is the stable identity; a handle
--      is a mutable label on it. That is this epic's whole thesis, and the
--      table says so rather than a comment in Rust.
--   2. The handle format is a CHECK, so every write is held to it no matter
--      which code path inserts — accessor, migration, or an operator with
--      sqlite3 open on the file.
--   3. Handle reuse is a PARTIAL UNIQUE INDEX. A handle is unique among
--      ACTIVE users only, so `detach` is an ordinary state change and the
--      retired row keeps the person's real handle.

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS user (
    -- Opaque ULID, and the filename stem of tenants/<database_id>.sqlite.
    -- The PRIMARY KEY: what a person's collection, their replica prefix and
    -- their history are all keyed on, and the one thing that never changes.
    database_id TEXT PRIMARY KEY,
    -- What a request names; renameable without touching anything on disk.
    -- The same charset paths::validate_tenant_name admits, spelled as
    -- a constraint so the database is the last word on it. The two cannot
    -- share an implementation — one is a Rust function, the other is checked
    -- for writers that never enter the crate — so they share a CORPUS instead:
    -- conformance/handles/cases.json is run through the validator by
    -- paths::tests::tenant_names_are_validated and through this CHECK by
    -- registry::tests::the_check_and_the_validator_agree. Relax one side only
    -- and one of those two fails. It matters because the validator is also
    -- what refuses a malformed handle in CLI arguments: a handle one side
    -- admits and the other does not is a command answered wrongly.
    --
    --   * 1..32 characters — long enough to name a person, short enough to
    --     stay a filename-safe label rather than a payload.
    --   * starts with a lowercase letter or digit — a leading `-` reads as a
    --     flag to every CLI it is passed to.
    --   * nothing outside a-z 0-9 - _ — no `.`, `/` or `\`, and no uppercase
    --     (on a case-insensitive filesystem `Alice` and `alice` are one file
    --     but two S3 prefixes).
    handle      TEXT NOT NULL CHECK (
        length(handle) BETWEEN 1 AND 32
        AND handle GLOB '[a-z0-9]*'            -- starts alphanumeric
        AND handle NOT GLOB '*[^a-z0-9_-]*'    -- and contains nothing else
        -- GLOB and length() both stop at an embedded NUL, so 'alice' || char(0) would pass
        -- the two lines above; its byte length is the one thing that still sees it.
        AND length(CAST(handle AS BLOB)) = length(handle)
    ),
    created_at  TEXT NOT NULL,
    -- active   — a live user; resolvable, and their handle is taken.
    -- detached — the handle was released but the database and its replica
    --            were kept. The row survives so the file stays attributable.
    state       TEXT NOT NULL CHECK (state IN ('active', 'detached')),
    -- When the handle was released. NULL while active.
    retired_at  TEXT
);

-- A handle is unique among ACTIVE users, and only among them.
--
-- This is what makes `detach` an ordinary state change instead of a primary
-- key rewrite: the retired row keeps the person's REAL handle, so an orphaned
-- database stays attributable without parsing a composite string, and the
-- handle is free for reuse the instant it is released. Two live users can
-- never share one, and the database is what says so.
CREATE UNIQUE INDEX IF NOT EXISTS user_one_active_handle
    ON user(handle) WHERE state = 'active';

-- ── KEY CUSTODY (pd-ulds) ──────────────────────────────────────────────────
--
-- Per-tenant encryption keys for the tenant zone are DERIVED, never stored:
-- HKDF(master key, database_id). So there is no key column here and never will
-- be — the only thing worth recording is whether this database's key may still
-- be derived at all. See the application's key-custody crate.
--
-- Three properties of this table are decisions, not implementation:
--
--   1. NO FOREIGN KEY to user(database_id). A tombstone must OUTLIVE the row
--      it names. `<app> tenant purge` deletes the user row; if that cascaded
--      to the tombstone, deletion would silently un-revoke the key it just
--      destroyed and the account would become readable again the moment the
--      partition was restored from anywhere. The tombstone is the durable
--      half of the deletion, so it deliberately does not depend on the
--      ephemeral half.
--   2. ABSENCE IS NOT PERMISSION. Derivation requires an explicit `active`
--      row; a database_id with no row here is refused (KeyError::NotRegistered)
--      rather than derived for. A registry restored empty is missing its
--      tombstones as well as its users, and the fail-closed direction is the
--      one where that is loud.
--   3. A TOMBSTONE IS TERMINAL. There is no path back to 'active' — not in
--      the accessor, and the CHECK below means an operator with sqlite3 open
--      on the file cannot half-do it either (clearing the state without
--      clearing tombstoned_at, or the reverse, fails the row constraint).
--      Revocation that can be undone by accident is not revocation.
--
-- What this table is NOT is the master key's business. The master key is a
-- file; this is a database. Backing the key up never touches a row here, and
-- tombstoning a tenant never touches that file — see the application's key-custody crate, where
-- the two paths are kept apart on purpose.
CREATE TABLE IF NOT EXISTS tenant_key (
    -- The database whose key state this is. Opaque ULID, exactly as
    -- user.database_id — but joined by value, never by constraint (1, above).
    database_id   TEXT PRIMARY KEY,
    -- active     — the key may be derived.
    -- tombstoned — it may not, ever again. Deliberate revocation.
    state         TEXT NOT NULL CHECK (state IN ('active', 'tombstoned')),
    -- When the id was first registered here (RFC 3339).
    created_at    TEXT NOT NULL,
    -- When the key was revoked. NULL while active, and the two move together.
    tombstoned_at TEXT,
    -- Whatever the operator said at the time. Free text, for the audit trail.
    reason        TEXT,
    CHECK ((state = 'tombstoned') = (tombstoned_at IS NOT NULL))
);

-- ── IDENTITY BINDINGS (db-dbb9) ───────────────────────────────────────────
--
-- One row per (verified identity, tenant) binding. A person proves who they
-- are by presenting a Cloudflare Access JWT; this table maps the email in
-- that JWT to the tenant they own.
--
-- Four design decisions, stated here rather than in the accessor alone:
--
--   1. BOUND TO database_id, NOT handle. A handle is a mutable label;
--      database_id is the stable identity. An identity bound to a handle
--      would follow a rename onto whoever takes that handle next.
--   2. KEYED ON email, sub STORED ALONGSIDE. The Access policy is written
--      in terms of email addresses; keying on email means the two systems
--      agree by construction. sub is kept for audit and for detecting a
--      provider change (same address, different sub), which should be
--      surfaced, not silently accepted or refused.
--   3. EMAIL NORMALISATION IS A CHECK. The stored value must already be
--      lowercase — `email = lower(email)` is enforced here, not by the
--      accessor alone. The accessor normalises before inserting; this check
--      holds every writer to it. Both are tested against conformance/emails/cases.json in
--      crate::registry so they cannot drift.
--   4. ONE EMAIL PER ACTIVE TENANT. Several identities per tenant (a person
--      with two addresses) is allowed; one identity mapping to several ACTIVE
--      tenants is refused — ambiguous at resolution time. The uniqueness index
--      is partial on user_state = 'active' so a detached tenant's email is
--      freed for re-onboarding. user_state mirrors user.state via the trigger
--      below.
--
-- Deletion obligation: when a tenant is deleted, their identity rows must
-- be removed too — they are personal data. the application's erase tooling handles the offline
-- half; registry::delete handles the online half. Neither may skip this.
CREATE TABLE IF NOT EXISTS user_identity (
    -- Stable; unchanged by renames.
    database_id  TEXT NOT NULL REFERENCES user(database_id),
    -- Normalised to lowercase. The human-readable anchor for a provider
    -- change: sub values are opaque and would orphan every binding on a
    -- provider migration.
    --
    -- The CHECK verifies that what is stored is already normalised — it does
    -- not normalise. The Rust normalise_email() lowercases before inserting;
    -- this check holds every writer to the same thing. Two implementations
    -- of "normalised" that cannot share code share conformance/emails/cases.json instead —
    -- the same pattern as conformance/handles/cases.json for handles.
    email        TEXT NOT NULL CHECK (
        length(email) >= 1
        AND email = lower(email)
    ),
    -- Identity provider subject. Opaque; stored for audit and for detecting
    -- a provider change (same address, different sub).
    sub          TEXT,
    -- Identity provider issuer URL.
    issuer       TEXT,
    created_at   TEXT NOT NULL,
    -- Mirror of user.state. Maintained by the trigger below, so a detach
    -- automatically frees the email for re-onboarding. The uniqueness index
    -- is partial on this column.
    user_state   TEXT NOT NULL CHECK (user_state IN ('active', 'detached')),
    PRIMARY KEY (database_id, email)
);

-- One email maps to at most ONE ACTIVE tenant.
--
-- Several identities per tenant is allowed (a person with two addresses).
-- One identity mapping to several ACTIVE tenants is refused — ambiguous at
-- resolution time, and that ambiguity must not be writable rather than
-- handled in code.
--
-- Partial on user_state = 'active' so that a detached user's email is freed
-- for re-onboarding: detach is an ordinary state change, not a deletion.
CREATE UNIQUE INDEX IF NOT EXISTS user_identity_one_active_per_email
    ON user_identity(email) WHERE user_state = 'active';

-- Keep user_state in sync with user.state. Fires when a user is detached
-- (state 'active' → 'detached'); the mirror is how the partial unique index
-- above stays accurate without re-reading the user table on every lookup.
CREATE TRIGGER IF NOT EXISTS user_identity_sync_state
AFTER UPDATE OF state ON user
BEGIN
    UPDATE user_identity SET user_state = NEW.state WHERE database_id = NEW.database_id;
END;
