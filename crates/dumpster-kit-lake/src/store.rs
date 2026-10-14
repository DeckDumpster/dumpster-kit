//! Where landed objects go, and where a rebuild reads them back from.
//!
//! Two traits with two implementations each: [`S3Store`], which is the real
//! one, and [`DirStore`], a directory on disk that exists so the landing
//! zone's behaviour can be asserted hermetically — the key layout, the
//! manifest and the never-overwrite property are all properties of the
//! *keys*, and proving them against a bucket would make the gate need
//! credentials and a network.
//!
//! The traits are separate on purpose. [`ObjectStore`] is what a *landing*
//! run holds, and it is still write-only: a run knows its own key space and
//! must not be able to look at anybody else's. [`ObjectSource`] is what the
//! offline derive holds, and it is read-only for the mirror-image reason —
//! `raw/` is immutable, and a job that could PUT into it could rewrite the
//! evidence it exists to preserve. Nothing implements both halves *as one
//! handle*; the two are constructed from the same config by different
//! entry points ([`crate::open`] and [`crate::open_reader`]).
//!
//! [`ObjectPurge`] is the third, and it is narrower than either: enumerate a
//! prefix, and remove one key. It exists for exactly one caller — the tenant
//! zone's deletion sweep (`pd-qbrf`) — and it deliberately has no `get` and
//! no `put`, because a job whose business is *removing* a tenant's holdings
//! has none reading them and none adding to them. It is handed out only by
//! [`crate::open_tenant_zone_purge`], never by [`crate::open`].

use std::path::{Path, PathBuf};

use crate::error::{LakeError, Result};

/// A write-only object store, keyed by string. Landing never reads and never
/// lists — a run knows its own key space and nothing else's.
pub trait ObjectStore: Send + Sync {
    /// Store `body` at `key`, creating or replacing.
    ///
    /// Replacement is possible at this layer and prevented at the one above:
    /// keys carry a per-run ULID, so no two runs address the same key. This
    /// trait deliberately does not offer a conditional put — the guarantee
    /// lives in the layout, where it can be tested without a bucket.
    fn put(&self, key: &str, body: Vec<u8>) -> Result<()>;

    /// A short description of where this store writes, for progress output.
    fn describe(&self) -> String;
}

/// A read-only view of a landing zone, keyed by string.
///
/// The offline derive is the only caller. It needs exactly two operations —
/// list the immediate children of a prefix (to find the `run=` directories a
/// date holds) and read one object whole — and deliberately not a third:
/// there is no `put`, so a job that reads `raw/` cannot write to it.
pub trait ObjectSource: Send + Sync {
    /// The bytes stored at `key`.
    ///
    /// Missing is an error rather than `None`: every key this trait is asked
    /// for came out of a manifest, so an absent object means the landing zone
    /// and its own record of itself disagree.
    fn get(&self, key: &str) -> Result<Vec<u8>>;

    /// The names of the immediate child "directories" of `key_prefix`, in no
    /// particular order, with no trailing slash. A prefix that does not exist
    /// is empty rather than an error — "this date landed nothing" is a fact
    /// the caller turns into its own refusal, with the date in it.
    fn child_dirs(&self, key_prefix: &str) -> Result<Vec<String>>;

    /// Every object key at or below `key_prefix`, **whole keys** rather than
    /// names, sorted.
    ///
    /// [`Self::child_dirs`] answers "which runs does this date hold"; a
    /// consumer of the tenant zone asks the other question — "which parts is
    /// this tenant's holdings dataset made of" — and it cannot know the
    /// answer in advance, because a part is named for the outbox range it
    /// carries (`dumpster_kit_lake::range_part_key`) and no reader knows which
    /// ranges were shipped.
    ///
    /// Sorted because the zone's part names are zero-padded for exactly that
    /// reason: sequence order out of a listing alone. An empty prefix is
    /// empty, not an error — a tenant who has shipped nothing has no parts,
    /// and that is a fact about them rather than a fault.
    fn list_keys(&self, key_prefix: &str) -> Result<Vec<String>>;

    /// A short description of where this source reads, for progress output.
    fn describe(&self) -> String;
}

/// Enumerate and remove objects. The deletion sweep's handle, and nothing
/// else's.
///
/// What makes it its own trait is `delete`, which neither of the others has
/// and neither should: [`ObjectStore`] writes and [`ObjectSource`] reads, and
/// a handle that could remove an object would make either one able to lose
/// the evidence it exists to keep. The absences are the design too — no
/// `get`, so the job whose business is *removing* a tenant's holdings has
/// nothing that reads them, and no `put`, so it has nothing that adds to
/// them.
///
/// `list` is deliberately **not** a second definition of "under this prefix".
/// It is [`ObjectSource::list_keys`], delegated to by every implementor, and
/// that is load-bearing rather than tidy: a sweep enumerating a prefix
/// differently from the reader that later checks it is exactly how a deletion
/// misses an object a reader can still see. One definition means the proof
/// and the deletion cannot disagree.
///
/// **This trait carries no scoping of its own.** `list` and `delete` will
/// address whatever key they are given; the prefix a caller is confined to is
/// the caller's business, and for the one caller there is that confinement is
/// `dumpster_kit_erase::sweep`, which refuses a key outside the single tenant prefix
/// it was constructed with. Putting the guard there rather than here is
/// deliberate: a trait that tried to be safe would need to know what
/// `tenant/` means, and this module is about bytes and keys.
pub trait ObjectPurge: Send + Sync {
    /// Every object key under `key_prefix`, recursively, in sorted order —
    /// the same answer [`ObjectSource::list_keys`] gives, and implementors
    /// are expected to delegate to it rather than re-derive it.
    ///
    /// Recursive, unlike [`ObjectSource::child_dirs`]: a tenant's partition is
    /// two levels of `key=value` deep and the sweep's whole claim is that
    /// nothing under the prefix survives, which is not a claim any single
    /// level can make. A prefix holding nothing is an empty list rather than
    /// an error — "already gone" is the normal second run.
    fn list(&self, key_prefix: &str) -> Result<Vec<String>>;

    /// Remove one object. Removing a key that is not there is **not** an
    /// error: deletion is re-run after a crash, and a sweep that failed on
    /// the objects it had already removed could never finish.
    fn delete(&self, key: &str) -> Result<()>;

    /// A short description of where this handle deletes from.
    fn describe(&self) -> String;
}

/// A directory on the local filesystem, one file per key.
///
/// Test-tier only. It is not a lake — nothing reads from it — but it holds
/// exactly the bytes and exactly the keys the S3 store would.
pub struct DirStore {
    root: PathBuf,
}

impl DirStore {
    /// Write objects under `root`, which is created on demand.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory this store writes into.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl ObjectStore for DirStore {
    fn put(&self, key: &str, body: Vec<u8>) -> Result<()> {
        let path = self.root.join(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, body)?;
        Ok(())
    }

    fn describe(&self) -> String {
        format!("dir {}", self.root.display())
    }
}

impl ObjectSource for DirStore {
    fn get(&self, key: &str) -> Result<Vec<u8>> {
        Ok(std::fs::read(self.root.join(key))?)
    }

    fn child_dirs(&self, key_prefix: &str) -> Result<Vec<String>> {
        let dir = self.root.join(key_prefix);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                out.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        Ok(out)
    }

    fn list_keys(&self, key_prefix: &str) -> Result<Vec<String>> {
        // A prefix, not a directory: `tenant/database_id=X/dataset=holdings/`
        // is a directory here, but S3 would answer the same call for a
        // prefix that stops mid-name. Walking from the deepest existing
        // ancestor and filtering keeps the two stores answering alike.
        let mut out = Vec::new();
        walk(&self.root, &self.root, &mut out)?;
        out.retain(|key| key.starts_with(key_prefix));
        out.sort();
        Ok(out)
    }

    fn describe(&self) -> String {
        format!("dir {}", self.root.display())
    }
}

impl ObjectPurge for DirStore {
    fn list(&self, key_prefix: &str) -> Result<Vec<String>> {
        ObjectSource::list_keys(self, key_prefix)
    }

    fn delete(&self, key: &str) -> Result<()> {
        match std::fs::remove_file(self.root.join(key)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn describe(&self) -> String {
        format!("dir {}", self.root.display())
    }
}

/// Every file under `dir`, as keys relative to `root`.
fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            walk(root, &path, out)?;
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_string_lossy().into_owned());
        }
    }
    Ok(())
}

/// The real landing zone: an S3 bucket.
///
/// Credentials come from the ambient AWS configuration chain — which, on
/// this project's boxes, means a profile whose `role_arn`/`source_profile`
/// the SDK assumes and **refreshes on its own**. That is the standing
/// credential decision for everything here, and it is why the AWS SDK is
/// worth its dependency weight over a hand-rolled SigV4 PUT: nothing in this
/// crate ever holds a long-lived key.
pub struct S3Store {
    client: aws_sdk_s3::Client,
    runtime: tokio::runtime::Runtime,
    bucket: String,
    prefix: String,
}

impl S3Store {
    /// Connect to `bucket` in `region`.
    ///
    /// `prefix` is prepended to every key (empty for none) and `endpoint`
    /// overrides the AWS endpoint, which is what lets a MinIO stand in for
    /// S3 in a container gate.
    pub fn connect(
        bucket: &str,
        region: &str,
        prefix: &str,
        endpoint: Option<&str>,
    ) -> Result<Self> {
        Self::connect_as(bucket, region, prefix, endpoint, None)
    }

    /// [`Self::connect`], under a named AWS profile.
    ///
    /// The tenant zone shares a bucket with the catalog and is separated from
    /// it by nothing but a pair of credential policies (`pd-uz8q`), so the
    /// identity a handle uses cannot be ambient — it is named, per handle, at
    /// the point the handle is built. Setting `AWS_PROFILE` in the process
    /// instead would make it a property of the *process*, and a process that
    /// touches both zones would then reach one of them with the other's role.
    ///
    /// Only the shared-config half of the chain is affected: an environment
    /// carrying explicit keys still wins, which is what lets a MinIO stand in
    /// for S3 in a gate that has no roles to assume.
    pub fn connect_as(
        bucket: &str,
        region: &str,
        prefix: &str,
        endpoint: Option<&str>,
        profile: Option<&str>,
    ) -> Result<Self> {
        // A current-thread runtime owned by the store: every caller in this
        // workspace's ingest path is blocking, and the alternative — making
        // the whole fetch path async for the sake of a PUT — would be a much
        // larger change than the feature warrants.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let region = aws_config::Region::new(region.to_string());
        let client = runtime.block_on(async {
            let mut loader =
                aws_config::defaults(aws_config::BehaviorVersion::latest()).region(region);
            if let Some(endpoint) = endpoint {
                loader = loader.endpoint_url(endpoint);
            }
            if let Some(profile) = profile {
                loader = loader.profile_name(profile);
            }
            let conf = loader.load().await;
            // Path-style addressing keeps a MinIO endpoint working; against
            // real S3 it is equivalent.
            let s3 = aws_sdk_s3::config::Builder::from(&conf)
                .force_path_style(endpoint.is_some())
                .build();
            aws_sdk_s3::Client::from_conf(s3)
        });

        Ok(Self {
            client,
            runtime,
            bucket: bucket.to_string(),
            prefix: normalize_prefix(prefix),
        })
    }

    fn full_key(&self, key: &str) -> String {
        format!("{}{}", self.prefix, key)
    }
}

impl ObjectStore for S3Store {
    fn put(&self, key: &str, body: Vec<u8>) -> Result<()> {
        let key = self.full_key(key);
        self.runtime.block_on(async {
            self.client
                .put_object()
                .bucket(&self.bucket)
                .key(&key)
                .body(body.into())
                .send()
                .await
                .map_err(|e| {
                    // The SDK's Display is a bare "service error"; the useful
                    // text is in the source chain.
                    LakeError::S3(format!(
                        "put s3://{}/{}: {}",
                        self.bucket,
                        key,
                        aws_error_text(&e)
                    ))
                })
        })?;
        Ok(())
    }

    fn describe(&self) -> String {
        format!("s3://{}/{}", self.bucket, self.prefix)
    }
}

impl ObjectSource for S3Store {
    fn get(&self, key: &str) -> Result<Vec<u8>> {
        let key = self.full_key(key);
        let body = self.runtime.block_on(async {
            let object = self
                .client
                .get_object()
                .bucket(&self.bucket)
                .key(&key)
                .send()
                .await
                .map_err(|e| {
                    LakeError::S3(format!(
                        "get s3://{}/{}: {}",
                        self.bucket,
                        key,
                        aws_error_text(&e)
                    ))
                })?;
            object
                .body
                .collect()
                .await
                .map_err(|e| LakeError::S3(format!("read s3://{}/{}: {e}", self.bucket, key)))
        })?;
        Ok(body.into_bytes().to_vec())
    }

    fn child_dirs(&self, key_prefix: &str) -> Result<Vec<String>> {
        // `Delimiter=/` makes S3 answer with CommonPrefixes — the object-store
        // equivalent of "the immediate children", and the reason this does not
        // have to list every part under a date to find its runs.
        let prefix = self.full_key(key_prefix);
        self.runtime.block_on(async {
            let mut out = Vec::new();
            let mut pages = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&prefix)
                .delimiter("/")
                .into_paginator()
                .send();
            while let Some(page) = pages.next().await {
                let page = page.map_err(|e| {
                    LakeError::S3(format!(
                        "list s3://{}/{}: {}",
                        self.bucket,
                        prefix,
                        aws_error_text(&e)
                    ))
                })?;
                for common in page.common_prefixes() {
                    if let Some(p) = common.prefix() {
                        out.push(
                            p.trim_end_matches('/')
                                .rsplit('/')
                                .next()
                                .unwrap_or_default()
                                .to_string(),
                        );
                    }
                }
            }
            Ok(out)
        })
    }

    fn list_keys(&self, key_prefix: &str) -> Result<Vec<String>> {
        // No delimiter: every object at or below the prefix, however deep.
        // The keys come back with `self.prefix` on them and go out without
        // it, because a caller that built the prefix with
        // `TenantZoneConfig::rooted` would otherwise get it twice.
        let prefix = self.full_key(key_prefix);
        let strip = self.prefix.clone();
        self.runtime.block_on(async {
            let mut out = Vec::new();
            let mut pages = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&prefix)
                .into_paginator()
                .send();
            while let Some(page) = pages.next().await {
                let page = page.map_err(|e| {
                    LakeError::S3(format!(
                        "list s3://{}/{}: {}",
                        self.bucket,
                        prefix,
                        aws_error_text(&e)
                    ))
                })?;
                for object in page.contents() {
                    if let Some(key) = object.key() {
                        out.push(key.strip_prefix(&strip).unwrap_or(key).to_string());
                    }
                }
            }
            out.sort();
            Ok(out)
        })
    }

    fn describe(&self) -> String {
        format!("s3://{}/{}", self.bucket, self.prefix)
    }
}

impl ObjectPurge for S3Store {
    fn list(&self, key_prefix: &str) -> Result<Vec<String>> {
        ObjectSource::list_keys(self, key_prefix)
    }

    fn delete(&self, key: &str) -> Result<()> {
        // S3's DELETE is already idempotent — removing an absent key answers
        // 204 — which is the behaviour this trait requires and the reason
        // there is no existence check here.
        let key = self.full_key(key);
        self.runtime.block_on(async {
            self.client
                .delete_object()
                .bucket(&self.bucket)
                .key(&key)
                .send()
                .await
                .map_err(|e| {
                    LakeError::S3(format!(
                        "delete s3://{}/{}: {}",
                        self.bucket,
                        key,
                        aws_error_text(&e)
                    ))
                })
        })?;
        Ok(())
    }

    fn describe(&self) -> String {
        format!("s3://{}/{}", self.bucket, self.prefix)
    }
}

/// Walk an SDK error's source chain — the outermost `Display` is rarely the
/// part that says what went wrong.
fn aws_error_text(err: &dyn std::error::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut source = err.source();
    while let Some(e) = source {
        parts.push(e.to_string());
        source = e.source();
    }
    parts.join(": ")
}

/// A key prefix, normalized to either empty or `something/`.
fn normalize_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim().trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_store_creates_nested_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let store = DirStore::new(tmp.path());
        store
            .put(
                "raw/source=tcgcsv/dataset=groups/x/part-0000.json.zst",
                b"hi".to_vec(),
            )
            .unwrap();
        assert_eq!(
            std::fs::read(
                tmp.path()
                    .join("raw/source=tcgcsv/dataset=groups/x/part-0000.json.zst")
            )
            .unwrap(),
            b"hi"
        );
    }

    /// The listing a zone reader works from: whole keys, sorted, and scoped
    /// to the prefix it asked about rather than to the store.
    #[test]
    fn dir_store_lists_whole_keys_under_a_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let store = DirStore::new(tmp.path());
        for key in [
            "tenant/database_id=B/dataset=holdings/as_of=2026-08-14/part-seq-000000000001-000000000002.parquet.enc",
            "tenant/database_id=A/dataset=holdings/as_of=2026-08-14/part-seq-000000000003-000000000004.parquet.enc",
            "tenant/database_id=A/dataset=holdings/as_of=2026-08-13/part-seq-000000000001-000000000002.parquet.enc",
            "raw/source=tcgcsv/dataset=groups/x/part-0000.json.zst",
        ] {
            store.put(key, b"x".to_vec()).unwrap();
        }

        let a = store.list_keys("tenant/database_id=A/").unwrap();
        assert_eq!(a.len(), 2, "A's two parts and nobody else's: {a:?}");
        assert!(a[0].contains("as_of=2026-08-13"), "sorted: {a:?}");
        assert!(a.iter().all(|k| k.contains("database_id=A/")));

        assert_eq!(store.list_keys("tenant/").unwrap().len(), 3);
        assert!(store.list_keys("tenant/database_id=C/").unwrap().is_empty());
    }

    /// The sweep's whole claim rests on this being recursive: a tenant
    /// partition is three `key=value` levels deep before an object appears.
    #[test]
    fn dir_store_lists_a_prefix_recursively_and_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let store = DirStore::new(tmp.path());
        for key in [
            "tenant/database_id=A/dataset=holdings/as_of=2026-08-14/part-0001",
            "tenant/database_id=A/dataset=holdings/as_of=2026-08-13/part-0000",
            "tenant/database_id=A/dataset=valuations/as_of=2026-08-14/part-0000",
            "tenant/database_id=B/dataset=holdings/as_of=2026-08-14/part-0000",
            "raw/source=tcgcsv/x",
        ] {
            store.put(key, b"x".to_vec()).unwrap();
        }

        assert_eq!(
            ObjectPurge::list(&store, "tenant/database_id=A/").unwrap(),
            vec![
                "tenant/database_id=A/dataset=holdings/as_of=2026-08-13/part-0000",
                "tenant/database_id=A/dataset=holdings/as_of=2026-08-14/part-0001",
                "tenant/database_id=A/dataset=valuations/as_of=2026-08-14/part-0000",
            ],
            "one tenant's prefix must cover every dataset and no other tenant"
        );
    }

    /// The sweep and the reader must mean the SAME thing by "under this
    /// prefix", including for a prefix that stops mid-name — the case the two
    /// implementations this merge reconciled disagreed on. `list` walking from
    /// `root.join(prefix)` answered *empty* here, because that path is neither
    /// a file nor a directory, while S3 and `list_keys` answer with every key
    /// the prefix matches. That gap runs in the one direction that matters: a
    /// sweep that under-reports deletes less than the proof later looks for.
    #[test]
    fn purge_lists_exactly_what_the_reader_lists() {
        let tmp = tempfile::tempdir().unwrap();
        let store = DirStore::new(tmp.path());
        for key in [
            "tenant/database_id=AB/dataset=holdings/as_of=2026-08-14/part-0000",
            "tenant/database_id=A/dataset=holdings/as_of=2026-08-14/part-0000",
        ] {
            store.put(key, b"x".to_vec()).unwrap();
        }

        // Stops mid-name: matches `database_id=A/…` and `database_id=AB/…` both.
        let prefix = "tenant/database_id=A";
        let listed = ObjectSource::list_keys(&store, prefix).unwrap();
        assert_eq!(
            listed.len(),
            2,
            "a key prefix is not a directory: {listed:?}"
        );
        assert_eq!(
            ObjectPurge::list(&store, prefix).unwrap(),
            listed,
            "the sweep must enumerate what the reader enumerates, exactly"
        );
    }

    /// A prefix nothing is under is an empty answer, not an error — the
    /// normal shape of the second run of a deletion.
    #[test]
    fn dir_store_lists_an_absent_prefix_as_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let store = DirStore::new(tmp.path());
        assert!(
            ObjectPurge::list(&store, "tenant/database_id=NOBODY/")
                .unwrap()
                .is_empty()
        );
    }

    /// Deleting twice is what a resumed sweep does on every object it already
    /// removed. It has to be free.
    #[test]
    fn dir_store_deletes_idempotently() {
        let tmp = tempfile::tempdir().unwrap();
        let store = DirStore::new(tmp.path());
        let key = "tenant/database_id=A/dataset=holdings/as_of=2026-08-14/part-0000";
        store.put(key, b"x".to_vec()).unwrap();
        store.delete(key).unwrap();
        store.delete(key).unwrap();
        assert!(ObjectPurge::list(&store, "tenant/").unwrap().is_empty());
    }

    #[test]
    fn prefix_normalizes_to_empty_or_trailing_slash() {
        assert_eq!(normalize_prefix(""), "");
        assert_eq!(normalize_prefix("   "), "");
        assert_eq!(normalize_prefix("/"), "");
        assert_eq!(normalize_prefix("lake"), "lake/");
        assert_eq!(normalize_prefix("/lake/"), "lake/");
        assert_eq!(normalize_prefix("a/b"), "a/b/");
    }
}
