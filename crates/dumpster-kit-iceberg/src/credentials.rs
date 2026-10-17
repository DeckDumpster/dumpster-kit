//! S3 credential resolution for the lake, the way the Python lake
//! (pyiceberg/boto3) resolves it on prod.
//!
//! Order, first match wins, and none of the explicit branches ever falls
//! through to the EC2 instance-metadata endpoint (absent on this host, where
//! it only ever times out):
//!
//! 1. an explicit role ARN, assumed via STS from the base identity: static
//!    keys, else the named profile, else environment keys then the default
//!    profile;
//! 2. a named profile (`role_arn` + `source_profile` chains included);
//! 3. static keys, handed to the object store as plain properties;
//! 4. nothing explicit: the ambient AWS default chain, bounded by a deadline
//!    so a dead chain is an error naming itself, not an io timeout.

// aws-config still exports ProfileFiles under its old name; the replacement
// type lives in a crate we do not depend on.
#![allow(deprecated)]

use std::time::Duration;

use aws_config::BehaviorVersion;
use aws_config::meta::credentials::CredentialsProviderChain;
use aws_config::profile::ProfileFileCredentialsProvider;
use aws_config::profile::profile_file::ProfileFiles;
use aws_config::provider_config::ProviderConfig;
use aws_config::sts::AssumeRoleProvider;
use aws_credential_types::Credentials;
use aws_credential_types::provider::{ProvideCredentials, SharedCredentialsProvider};
use aws_smithy_runtime_api::client::http::SharedHttpClient;
use iceberg_storage_opendal::{AwsCredential, CustomAwsCredentialLoader, ProvideCredential};
use reqsign_core::{Context, Error as SignError, ErrorKind, Result as SignResult};

/// Session name on assumed roles, visible in CloudTrail.
pub const SESSION_NAME: &str = "dumpster-kit-iceberg";
/// How long credential resolution may take before it is reported as failed.
pub const RESOLVE_DEADLINE: Duration = Duration::from_secs(20);

/// The identity the lake's object store is reached with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct S3Identity {
    pub role_arn: Option<String>,
    pub profile: Option<String>,
    pub region: Option<String>,
    /// Both an access key id and a secret were configured.
    pub static_keys: Option<(String, Option<String>)>,
}

/// What `open` should do about credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Static keys go straight to the object store; nothing to resolve.
    StaticKeys,
    AssumeRole,
    Profile,
    Ambient,
}

impl S3Identity {
    pub fn plan(&self) -> Plan {
        if self.role_arn.is_some() {
            Plan::AssumeRole
        } else if self.profile.is_some() {
            Plan::Profile
        } else if self.static_keys.is_some() {
            Plan::StaticKeys
        } else {
            Plan::Ambient
        }
    }
}

/// Test seams: the HTTP client STS is called through and the profile files read.
#[derive(Default, Clone)]
pub struct Seams {
    pub http_client: Option<SharedHttpClient>,
    pub profile_files: Option<ProfileFiles>,
}

fn profile_provider(identity: &S3Identity, seams: &Seams) -> ProfileFileCredentialsProvider {
    let mut cfg = ProviderConfig::default();
    if let Some(h) = &seams.http_client {
        cfg = cfg.with_http_client(h.clone());
    }
    if let Some(r) = &identity.region {
        cfg = cfg.with_region(Some(aws_config::Region::new(r.clone())));
    }
    let mut b = ProfileFileCredentialsProvider::builder().configure(&cfg);
    if let Some(p) = &identity.profile {
        b = b.profile_name(p.clone());
    }
    if let Some(f) = &seams.profile_files {
        b = b.profile_files(f.clone());
    }
    b.build()
}

/// The credential provider for `identity`, or `None` when static keys are
/// handed to the object store directly. `Plan::Ambient` yields the AWS default
/// chain.
pub async fn provider(identity: &S3Identity, seams: &Seams) -> Option<SharedCredentialsProvider> {
    match identity.plan() {
        Plan::StaticKeys => None,
        Plan::Profile => Some(SharedCredentialsProvider::new(profile_provider(
            identity, seams,
        ))),
        Plan::Ambient => {
            let mut l = aws_config::defaults(BehaviorVersion::latest());
            if let Some(h) = &seams.http_client {
                l = l.http_client(h.clone());
            }
            if let Some(r) = &identity.region {
                l = l.region(aws_config::Region::new(r.clone()));
            }
            l.load().await.credentials_provider()
        }
        Plan::AssumeRole => {
            let arn = identity.role_arn.clone().unwrap_or_default();
            let base: SharedCredentialsProvider = match &identity.static_keys {
                Some((id, Some(secret))) => SharedCredentialsProvider::new(Credentials::new(
                    id.clone(),
                    secret.clone(),
                    None,
                    None,
                    "dumpster-kit-iceberg-static",
                )),
                _ if identity.profile.is_some() => {
                    SharedCredentialsProvider::new(profile_provider(identity, seams))
                }
                // Environment keys, then the default profile — never IMDS.
                _ => SharedCredentialsProvider::new(
                    CredentialsProviderChain::first_try(
                        "Environment",
                        aws_config::environment::EnvironmentVariableCredentialsProvider::new(),
                    )
                    .or_else("Profile", profile_provider(identity, seams)),
                ),
            };
            let mut l = aws_config::defaults(BehaviorVersion::latest()).no_credentials();
            if let Some(h) = &seams.http_client {
                l = l.http_client(h.clone());
            }
            if let Some(r) = &identity.region {
                l = l.region(aws_config::Region::new(r.clone()));
            }
            let conf = l.load().await;
            let assumed = AssumeRoleProvider::builder(arn)
                .session_name(SESSION_NAME)
                .configure(&conf)
                .build_from_provider(base)
                .await;
            Some(SharedCredentialsProvider::new(assumed))
        }
    }
}

/// Adapts an AWS SDK provider to the object store's credential trait, turning
/// a dead chain into an error that names what was tried.
#[derive(Debug)]
pub struct SdkCredentials {
    inner: SharedCredentialsProvider,
    what: String,
    deadline: Duration,
}

impl SdkCredentials {
    pub fn new(inner: SharedCredentialsProvider, identity: &S3Identity) -> Self {
        let what = match identity.plan() {
            Plan::AssumeRole => format!(
                "assuming role {}",
                identity.role_arn.as_deref().unwrap_or_default()
            ),
            Plan::Profile => format!(
                "AWS profile {}",
                identity.profile.as_deref().unwrap_or_default()
            ),
            _ => "the ambient AWS credential chain (PKDUMP_LAKE_S3_ROLE_ARN, PKDUMP_LAKE_S3_PROFILE/AWS_PROFILE and S3 access keys are all unset)".to_string(),
        };
        Self {
            inner,
            what,
            deadline: RESOLVE_DEADLINE,
        }
    }

    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    pub fn into_loader(self) -> CustomAwsCredentialLoader {
        CustomAwsCredentialLoader::new(self)
    }
}

impl ProvideCredential for SdkCredentials {
    type Credential = AwsCredential;

    async fn provide_credential(&self, _: &Context) -> SignResult<Option<AwsCredential>> {
        let fail = |kind, why: String| {
            SignError::new(kind, format!("no S3 credentials from {}: {why}", self.what))
        };
        let creds = tokio::time::timeout(self.deadline, self.inner.provide_credentials())
            .await
            .map_err(|_| {
                fail(
                    ErrorKind::ConfigInvalid,
                    format!("timed out after {}s", self.deadline.as_secs()),
                )
            })?
            .map_err(|e| fail(ErrorKind::CredentialInvalid, e.to_string()))?;
        let expires_in = match creds.expiry() {
            Some(t) => {
                let ms = t
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as i64);
                Some(
                    reqsign_core::time::Timestamp::from_millisecond(ms)
                        .map_err(|e| fail(ErrorKind::Unexpected, e.to_string()))?,
                )
            }
            None => None,
        };
        Ok(Some(AwsCredential {
            access_key_id: creds.access_key_id().to_string(),
            secret_access_key: creds.secret_access_key().to_string(),
            session_token: creds.session_token().map(str::to_string),
            expires_in,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_smithy_runtime::client::http::test_util::{ReplayEvent, StaticReplayClient};
    use aws_smithy_types::body::SdkBody;
    use std::io::Write;

    const ASSUMED: &str = r#"<AssumeRoleResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/">
  <AssumeRoleResult>
    <AssumedRoleUser><AssumedRoleId>AROAEXAMPLE:s</AssumedRoleId><Arn>arn:aws:sts::1:assumed-role/r/s</Arn></AssumedRoleUser>
    <Credentials>
      <AccessKeyId>ASIAASSUMED</AccessKeyId>
      <SecretAccessKey>assumed-secret</SecretAccessKey>
      <SessionToken>assumed-token</SessionToken>
      <Expiration>2099-01-01T00:00:00Z</Expiration>
    </Credentials>
  </AssumeRoleResult>
</AssumeRoleResponse>"#;

    fn fake_sts() -> (StaticReplayClient, Seams) {
        let client = StaticReplayClient::new(vec![ReplayEvent::new(
            http::Request::builder().body(SdkBody::empty()).unwrap(),
            http::Response::builder()
                .status(200)
                .body(SdkBody::from(ASSUMED))
                .unwrap(),
        )]);
        let seams = Seams {
            http_client: Some(SharedHttpClient::new(client.clone())),
            profile_files: None,
        };
        (client, seams)
    }

    fn sent_body(client: &StaticReplayClient) -> String {
        let reqs = client.actual_requests().collect::<Vec<_>>();
        assert_eq!(reqs.len(), 1, "exactly one STS call, no IMDS");
        String::from_utf8_lossy(reqs[0].body().bytes().unwrap()).into_owned()
    }

    fn ident() -> S3Identity {
        S3Identity {
            region: Some("us-east-1".into()),
            ..Default::default()
        }
    }

    #[test]
    fn plan_precedence() {
        let keys = Some(("k".to_string(), Some("s".to_string())));
        let mut i = ident();
        assert_eq!(i.plan(), Plan::Ambient);
        i.static_keys = keys;
        assert_eq!(i.plan(), Plan::StaticKeys);
        i.profile = Some("p".into());
        assert_eq!(i.plan(), Plan::Profile);
        i.role_arn = Some("arn:aws:iam::1:role/r".into());
        assert_eq!(i.plan(), Plan::AssumeRole);
    }

    #[tokio::test]
    async fn role_arn_is_assumed_via_sts_from_static_keys() {
        let (client, seams) = fake_sts();
        let id = S3Identity {
            role_arn: Some("arn:aws:iam::1:role/lake".into()),
            static_keys: Some(("AKIABASE".into(), Some("base-secret".into()))),
            ..ident()
        };
        let p = provider(&id, &seams).await.expect("provider");
        let c = SdkCredentials::new(p, &id)
            .provide_credential(&Context::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(c.access_key_id, "ASIAASSUMED");
        assert_eq!(c.session_token.as_deref(), Some("assumed-token"));
        let body = sent_body(&client);
        assert!(
            body.contains("RoleArn=arn%3Aaws%3Aiam%3A%3A1%3Arole%2Flake"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn profile_role_arn_source_profile_chain_is_followed() {
        let dir = std::env::temp_dir().join(format!("dk-iceberg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(
            f,
            "[profile base]\naws_access_key_id = AKIABASE\naws_secret_access_key = base-secret\n\n\
             [profile prod]\nrole_arn = arn:aws:iam::1:role/prod\nsource_profile = base\n"
        )
        .unwrap();
        let (client, mut seams) = fake_sts();
        seams.profile_files = Some(
            ProfileFiles::builder()
                .with_file(
                    aws_config::profile::profile_file::ProfileFileKind::Config,
                    &path,
                )
                .include_default_credentials_file(false)
                .include_default_config_file(false)
                .build(),
        );
        let id = S3Identity {
            profile: Some("prod".into()),
            ..ident()
        };
        let p = provider(&id, &seams).await.expect("provider");
        let c = SdkCredentials::new(p, &id)
            .provide_credential(&Context::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(c.access_key_id, "ASIAASSUMED");
        assert!(sent_body(&client).contains("role%2Fprod"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn static_keys_need_no_provider() {
        let id = S3Identity {
            static_keys: Some(("k".into(), Some("s".into()))),
            ..ident()
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        assert!(rt.block_on(provider(&id, &Seams::default())).is_none());
    }

    #[tokio::test]
    async fn a_dead_chain_is_a_fast_error_naming_what_was_tried() {
        #[derive(Debug)]
        struct Never;
        impl ProvideCredentials for Never {
            fn provide_credentials<'a>(
                &'a self,
            ) -> aws_credential_types::provider::future::ProvideCredentials<'a>
            where
                Self: 'a,
            {
                aws_credential_types::provider::future::ProvideCredentials::new(async {
                    std::future::pending().await
                })
            }
        }
        let id = S3Identity {
            role_arn: Some("arn:aws:iam::1:role/lake".into()),
            ..ident()
        };
        let err = SdkCredentials::new(SharedCredentialsProvider::new(Never), &id)
            .with_deadline(Duration::from_millis(50))
            .provide_credential(&Context::new())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("assuming role arn:aws:iam::1:role/lake"),
            "{err}"
        );
        assert!(err.contains("timed out"), "{err}");
    }
}
