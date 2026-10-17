use std::collections::HashMap;
use std::sync::Arc;

use iceberg::CatalogBuilder;
use iceberg_catalog_rest::{REST_CATALOG_PROP_URI, RestCatalog, RestCatalogBuilder};
use iceberg_storage_opendal::OpenDalStorageFactory;

use crate::credentials::{self, Plan, S3Identity, SdkCredentials, Seams};
use crate::{Error, Result};

/// Nessie's Iceberg REST endpoint, e.g. `http://nessie:19120/iceberg/`.
pub const URI_ENV: &str = "PKDUMP_LAKE_NESSIE_URI";
/// The Nessie ref to read and write: a branch, a tag, or `main@<hash>`.
pub const REF_ENV: &str = "PKDUMP_LAKE_REF";
pub const DEFAULT_REF: &str = "main";

pub const ROLE_ARN_ENV: &str = "PKDUMP_LAKE_S3_ROLE_ARN";
pub const PROFILE_ENV: &str = "PKDUMP_LAKE_S3_PROFILE";

/// `(iceberg property, env var)` for the S3 overrides the Python lake honours.
const S3_ENV: &[(&str, &str)] = &[
    ("s3.endpoint", "PKDUMP_LAKE_S3_ENDPOINT"),
    ("s3.access-key-id", "PKDUMP_LAKE_S3_ACCESS_KEY_ID"),
    ("s3.secret-access-key", "PKDUMP_LAKE_S3_SECRET_ACCESS_KEY"),
    ("s3.region", "PKDUMP_LAKE_S3_REGION"),
];

/// How to reach the lake, resolved from the environment or built directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LakeCatalogConfig {
    pub uri: String,
    /// Nessie ref, passed as the REST `prefix`.
    pub reference: String,
    pub s3: Vec<(String, String)>,
    pub path_style: bool,
    /// Role to assume via STS from the base identity (see [`crate::credentials`]).
    pub role_arn: Option<String>,
    /// Named AWS profile; falls back to `AWS_PROFILE`.
    pub profile: Option<String>,
}

impl LakeCatalogConfig {
    /// Resolve from `lookup` (the environment in production). No fallback for
    /// the endpoint; the ref defaults to `main`.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let get = |k: &str| lookup(k).filter(|v| !v.is_empty());
        let uri = get(URI_ENV).ok_or(Error::MissingEnv(
            URI_ENV,
            "Nessie's Iceberg REST endpoint, e.g. http://nessie:19120/iceberg/",
        ))?;
        Ok(Self {
            uri,
            reference: get(REF_ENV).unwrap_or_else(|| DEFAULT_REF.to_string()),
            s3: S3_ENV
                .iter()
                .filter_map(|(p, e)| get(e).map(|v| (p.to_string(), v)))
                .collect(),
            path_style: get("PKDUMP_LAKE_S3_PATH_STYLE").is_some(),
            role_arn: get(ROLE_ARN_ENV),
            profile: get(PROFILE_ENV).or_else(|| get("AWS_PROFILE")),
        })
    }

    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// The identity the object store is reached with.
    pub fn identity(&self) -> S3Identity {
        let prop = |k: &str| self.s3.iter().find(|(p, _)| p == k).map(|(_, v)| v.clone());
        S3Identity {
            role_arn: self.role_arn.clone(),
            profile: self.profile.clone(),
            region: prop("s3.region"),
            static_keys: prop("s3.access-key-id").map(|id| (id, prop("s3.secret-access-key"))),
        }
    }

    /// The property map handed to the REST catalog.
    pub fn properties(&self) -> HashMap<String, String> {
        let mut props = HashMap::from([
            (REST_CATALOG_PROP_URI.to_string(), self.uri.clone()),
            ("prefix".to_string(), self.reference.clone()),
        ]);
        // Static keys are the base identity of a role assumption, not the
        // credential the object store should sign with.
        let keys_signed = self.identity().plan() == Plan::StaticKeys;
        props.extend(
            self.s3
                .iter()
                .filter(|(p, _)| {
                    keys_signed || !p.starts_with("s3.access-key") && !p.starts_with("s3.secret")
                })
                .cloned(),
        );
        if self.path_style {
            props.insert("s3.path-style-access".into(), "true".into());
        }
        props
    }
}

/// Open the lake catalog.
pub async fn open(config: &LakeCatalogConfig) -> Result<RestCatalog> {
    let identity = config.identity();
    let customized_credential_load = credentials::provider(&identity, &Seams::default())
        .await
        .map(|p| SdkCredentials::new(p, &identity).into_loader());
    let factory = Arc::new(OpenDalStorageFactory::S3 {
        customized_credential_load,
    });
    Ok(RestCatalogBuilder::default()
        .with_storage_factory(factory)
        .load("lake", config.properties())
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(pairs: &[(&str, &str)]) -> Result<LakeCatalogConfig> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        LakeCatalogConfig::from_lookup(|k| m.get(k).cloned())
    }

    #[test]
    fn endpoint_has_no_fallback() {
        assert!(matches!(cfg(&[]), Err(Error::MissingEnv(URI_ENV, _))));
        assert!(cfg(&[(URI_ENV, "")]).is_err());
    }

    #[test]
    fn ref_defaults_to_main_and_becomes_the_prefix() {
        let c = cfg(&[(URI_ENV, "http://n/iceberg/")]).unwrap();
        assert_eq!(c.properties()["prefix"], "main");
        let c = cfg(&[(URI_ENV, "http://n/iceberg/"), (REF_ENV, "main@abc")]).unwrap();
        assert_eq!(c.properties()["prefix"], "main@abc");
    }

    #[test]
    fn s3_overrides_pass_through() {
        let c = cfg(&[
            (URI_ENV, "http://n/iceberg/"),
            ("PKDUMP_LAKE_S3_ENDPOINT", "http://minio:9000"),
            ("PKDUMP_LAKE_S3_PATH_STYLE", "1"),
        ])
        .unwrap();
        let p = c.properties();
        assert_eq!(p["s3.endpoint"], "http://minio:9000");
        assert_eq!(p["s3.path-style-access"], "true");
        assert!(!p.contains_key("s3.region"));
    }

    #[test]
    fn role_arn_drops_static_keys_from_the_signing_props() {
        let c = cfg(&[
            (URI_ENV, "http://n/iceberg/"),
            (ROLE_ARN_ENV, "arn:aws:iam::1:role/r"),
            ("PKDUMP_LAKE_S3_ACCESS_KEY_ID", "AKIA"),
            ("PKDUMP_LAKE_S3_SECRET_ACCESS_KEY", "s"),
        ])
        .unwrap();
        assert_eq!(c.identity().plan(), Plan::AssumeRole);
        assert!(!c.properties().contains_key("s3.access-key-id"));
        let c = cfg(&[
            (URI_ENV, "http://n/iceberg/"),
            ("PKDUMP_LAKE_S3_ACCESS_KEY_ID", "AKIA"),
        ])
        .unwrap();
        assert_eq!(c.properties()["s3.access-key-id"], "AKIA");
    }
}
