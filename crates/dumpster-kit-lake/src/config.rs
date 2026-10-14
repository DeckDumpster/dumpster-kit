//! Where the lake lives — read, never guessed.
//!
//! The bucket is **host configuration**, not a repo constant, exactly like
//! the Litestream bucket and the container store before it. It comes from
//! `~/.config/dumpstar/lake.env` in the same dotenv shape as `alerts.env`,
//! `litestream.env` and `store.env`, and an explicit environment variable
//! beats the file (the `store.env` precedent).
//!
//! If it is not configured, landing **refuses and names the file**. It does
//! not fall back to a default bucket, and it does not quietly skip the
//! landing step and let a refresh report success — the whole point of the
//! landing zone is that the bytes are there afterwards, so "configured
//! wrong" and "did nothing" must not look alike.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dumpster_kit_tenancy::AppPrefix;

use crate::error::{LakeError, Result};

/// The names this crate reads, derived from one [`AppPrefix`]: a site's
/// `PKDUMP` yields `PKDUMP_LAKE_S3_BUCKET` and `~/.config/pkdump/lake.env`, so
/// two sites on one box cannot read each other's lake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LakeEnv {
    /// The bucket name. Host config; there is no default and never will be.
    pub bucket: String,
    /// The bucket's region.
    pub region: String,
    /// A key prefix inside the bucket. Optional; empty means `raw/` sits at
    /// the bucket root.
    pub prefix: String,
    /// An S3 endpoint override — how a MinIO stands in for the real bucket.
    pub endpoint: String,
    /// A local directory to land into instead of a bucket. Test tier only; it
    /// takes precedence over the S3 settings when both are present.
    pub dir: String,
    /// Redirects the config file itself, so a test never reads — or writes —
    /// the operator's real one.
    pub env_file: String,
    /// The credential identity the tenant zone is reached with.
    pub tenant_profile: String,
    /// Overrides the tenant zone root for a stand-in bucket. Test tier only.
    pub tenant_prefix: String,
    /// The default location of the config file, relative to `$HOME`.
    pub config_relative_path: String,
}

impl LakeEnv {
    /// The names for `app`.
    pub fn new(app: &AppPrefix) -> Self {
        Self {
            bucket: app.var("LAKE_S3_BUCKET"),
            region: app.var("LAKE_S3_REGION"),
            prefix: app.var("LAKE_S3_PREFIX"),
            endpoint: app.var("LAKE_S3_ENDPOINT"),
            dir: app.var("LAKE_DIR"),
            env_file: app.var("LAKE_ENV"),
            tenant_profile: app.var("TENANT_AWS_PROFILE"),
            tenant_prefix: app.var("TENANT_S3_PREFIX"),
            config_relative_path: format!(".config/{}/lake.env", app.cli()),
        }
    }

    /// The config file this process would read, honouring `env_file`.
    /// `None` when there is no `$HOME` to resolve the default against.
    pub fn config_path(&self) -> Option<PathBuf> {
        if let Ok(explicit) = std::env::var(&self.env_file) {
            return Some(PathBuf::from(explicit));
        }
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(&self.config_relative_path))
    }
}

/// The catalog side's identity, for the one comparison that matters: the
/// tenant zone's must not be the same. Read, never written.
pub const KEY_CATALOG_PROFILE: &str = "AWS_PROFILE";

/// Where landed objects go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    /// The real thing.
    S3 {
        /// Bucket name.
        bucket: String,
        /// Bucket region.
        region: String,
        /// Key prefix, possibly empty.
        prefix: String,
        /// Endpoint override, for a MinIO stand-in.
        endpoint: Option<String>,
    },
    /// A directory on disk — hermetic gates only.
    Dir(PathBuf),
}

/// A resolved landing-zone configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LakeConfig {
    /// Where to write.
    pub backend: Backend,
}

impl LakeConfig {
    /// Resolve from the process environment layered over
    /// `~/.config/dumpstar/lake.env`, environment winning.
    ///
    /// Fails, naming the file, when nothing configures a destination.
    pub fn load(env: &LakeEnv) -> Result<Self> {
        let path = env.config_path();
        let mut settings = match &path {
            Some(p) => read_env_file(p)?,
            None => BTreeMap::new(),
        };
        for key in [
            &env.bucket,
            &env.region,
            &env.prefix,
            &env.endpoint,
            &env.dir,
        ] {
            if let Ok(value) = std::env::var(key) {
                settings.insert(key.to_string(), value);
            }
        }
        Self::from_settings(env, &settings, path.as_deref())
    }

    /// Resolve from an explicit settings map. `config_path` only shapes the
    /// error message — it is the file an operator would have to write.
    pub fn from_settings(
        env: &LakeEnv,
        settings: &BTreeMap<String, String>,
        config_path: Option<&Path>,
    ) -> Result<Self> {
        let get = |k: &str| {
            settings
                .get(k)
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };

        if let Some(dir) = get(&env.dir) {
            return Ok(Self {
                backend: Backend::Dir(PathBuf::from(dir)),
            });
        }

        let where_from = match config_path {
            Some(p) => p.display().to_string(),
            None => format!("~/{}", env.config_relative_path),
        };

        let (Some(bucket), Some(region)) = (get(&env.bucket), get(&env.region)) else {
            return Err(LakeError::NotConfigured(format!(
                "the raw landing zone is not configured: {where_from} is missing or does not set \
                 {} and {}.\n\
                 The lake bucket is host configuration and has no default — it is deliberately a \
                 separate bucket from the Litestream backup bucket, because the backups are the \
                 only irreplaceable data in the system and a lifecycle rule written for one must \
                 never be able to reach the other.\n\
                 Create the bucket, then write {where_from}:\n\
                 \x20 {}=<bucket>\n\
                 \x20 {}=<region>\n\
                 \x20 #{}=          # optional key prefix\n\
                 \x20 #{}=        # optional, for a MinIO stand-in",
                env.bucket, env.region, env.bucket, env.region, env.prefix, env.endpoint
            )));
        };

        Ok(Self {
            backend: Backend::S3 {
                bucket,
                region,
                prefix: get(&env.prefix).unwrap_or_default(),
                endpoint: get(&env.endpoint),
            },
        })
    }
}

/// Parse a dotenv file. Missing is not an error — it is simply no settings,
/// which the caller turns into the refusal above.
pub(crate) fn read_env_file(path: &Path) -> Result<BTreeMap<String, String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(parse_env(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e.into()),
    }
}

/// The same subset of dotenv the deploy scripts' `set -a; . file` handles:
/// `KEY=value`, `#` comments, blank lines, an optional `export ` prefix, and
/// optional surrounding quotes.
pub(crate) fn parse_env(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        out.insert(key.trim().to_string(), value.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> LakeEnv {
        LakeEnv::new(&AppPrefix::new("TESTAPP").unwrap())
    }

    fn settings(pairs: &[(&String, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parses_the_deploy_scripts_dotenv_subset() {
        let parsed = parse_env(
            "# the lake bucket\n\
             \n\
             TESTAPP_LAKE_S3_BUCKET=testapp-lake-1\n\
             export TESTAPP_LAKE_S3_REGION=\"us-west-2\"\n\
             TESTAPP_LAKE_S3_PREFIX='sub/dir'\n\
             #TESTAPP_LAKE_S3_ENDPOINT=\n",
        );
        let env = env();
        assert_eq!(parsed.get(&env.bucket).unwrap(), "testapp-lake-1");
        assert_eq!(parsed.get(&env.region).unwrap(), "us-west-2");
        assert_eq!(parsed.get(&env.prefix).unwrap(), "sub/dir");
        assert!(!parsed.contains_key(&env.endpoint));
    }

    #[test]
    fn two_apps_read_different_names() {
        let a = LakeEnv::new(&AppPrefix::new("PKDUMP").unwrap());
        let b = LakeEnv::new(&AppPrefix::new("DUMPSTAR").unwrap());
        assert_eq!(a.bucket, "PKDUMP_LAKE_S3_BUCKET");
        assert_eq!(b.config_relative_path, ".config/dumpstar/lake.env");
        assert_ne!(a.env_file, b.env_file);
    }

    #[test]
    fn resolves_an_s3_backend() {
        let cfg = LakeConfig::from_settings(
            &env(),
            &settings(&[
                (&env().bucket, "testapp-lake-1"),
                (&env().region, "us-west-2"),
                (&env().prefix, "lake"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(
            cfg.backend,
            Backend::S3 {
                bucket: "testapp-lake-1".into(),
                region: "us-west-2".into(),
                prefix: "lake".into(),
                endpoint: None,
            }
        );
    }

    /// The bead's standing decision, executable: an absent or half-written
    /// `lake.env` refuses and says which file to write. Nothing is invented.
    #[test]
    fn an_unconfigured_lake_refuses_and_names_the_file() {
        let err = LakeConfig::from_settings(
            &env(),
            &BTreeMap::new(),
            Some(Path::new("/home/someone/.config/testapp/lake.env")),
        )
        .unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("/home/someone/.config/testapp/lake.env"),
            "{text}"
        );
        assert!(text.contains(&env().bucket), "{text}");
        assert!(!text.to_lowercase().contains("default bucket"), "{text}");

        // A region with no bucket is just as unconfigured.
        assert!(
            LakeConfig::from_settings(&env(), &settings(&[(&env().region, "us-west-2")]), None)
                .is_err()
        );
        // …and an empty value is not a value.
        assert!(
            LakeConfig::from_settings(
                &env(),
                &settings(&[(&env().bucket, "  "), (&env().region, "us-west-2")]),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn a_directory_backend_wins_over_s3_settings() {
        let cfg = LakeConfig::from_settings(
            &env(),
            &settings(&[
                (&env().dir, "/tmp/lake"),
                (&env().bucket, "testapp-lake-1"),
                (&env().region, "us-west-2"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(cfg.backend, Backend::Dir(PathBuf::from("/tmp/lake")));
    }

    #[test]
    fn a_missing_file_is_no_settings_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let parsed = read_env_file(&tmp.path().join("absent.env")).unwrap();
        assert!(parsed.is_empty());
    }
}
