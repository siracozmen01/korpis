// SPDX-License-Identifier: AGPL-3.0-or-later

//! The store's token service: `AssumeRole` with an inline session policy, which answers a
//! temporary credential allowed what both the policy and the provider's own credential allow, for
//! as long as asked. AWS needs the role to assume; MinIO assumes the caller's own identity and
//! ignores it.

use instant_xml::FromXml;
use korpis_extension::sigv4::{Request, Signer};
use std::time::{Duration, SystemTime};

const STS_NS: &str = "https://sts.amazonaws.com/doc/2011-06-15/";

/// Why the token service gave nothing.
#[derive(Debug, thiserror::Error)]
pub enum StsError {
    /// It answered, refusing.
    #[error("the token service refused: {0}")]
    Refused(String),
    /// It could not be reached, or its answer could not be read.
    #[error("the token service failed: {0}")]
    Failed(String),
}

/// A temporary credential.
#[derive(Clone, PartialEq, Eq)]
pub struct Temporary {
    /// The access key.
    pub access_key_id: String,
    /// Its secret.
    pub secret_access_key: String,
    /// The session it belongs to.
    pub session_token: String,
    /// When it ends.
    pub expires: SystemTime,
}

impl std::fmt::Debug for Temporary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Temporary")
            .field("access_key_id", &self.access_key_id)
            .field("expires", &self.expires)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, FromXml)]
#[xml(rename = "AssumeRoleResponse", ns(STS_NS))]
struct AssumeRoleResponse {
    result: AssumeRoleResult,
}

#[derive(Debug, FromXml)]
#[xml(rename = "AssumeRoleResult", ns(STS_NS))]
struct AssumeRoleResult {
    credentials: Credentials,
}

#[derive(Debug, FromXml)]
#[xml(rename = "Credentials", ns(STS_NS))]
struct Credentials {
    #[xml(rename = "AccessKeyId")]
    access_key_id: String,
    #[xml(rename = "SecretAccessKey")]
    secret_access_key: String,
    #[xml(rename = "SessionToken")]
    session_token: String,
    #[xml(rename = "Expiration")]
    expiration: String,
}

/// The token service, called with the provider's own credential.
#[derive(Clone)]
pub struct Sts {
    /// Where it is.
    pub endpoint: url::Url,
    /// The region it signs for.
    pub region: String,
    /// The provider's access key.
    pub access_key_id: String,
    /// Its secret.
    pub secret: String,
    /// The role assumed, where the service needs one.
    pub role_arn: Option<String>,
    /// The client.
    pub http: reqwest::Client,
}

impl std::fmt::Debug for Sts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sts")
            .field("endpoint", &self.endpoint.as_str())
            .field("region", &self.region)
            .field("access_key_id", &self.access_key_id)
            .finish_non_exhaustive()
    }
}

/// `at` as the service's `x-amz-date`.
fn amz_date(at: SystemTime) -> Result<String, StsError> {
    let t = jiff::Timestamp::try_from(at).map_err(|e| StsError::Failed(e.to_string()))?;
    Ok(t.strftime("%Y%m%dT%H%M%SZ").to_string())
}

/// The `host` header of `url`, with its port unless it is the scheme's own.
fn host(url: &url::Url) -> String {
    let name = url.host_str().unwrap_or_default();
    url.port().map_or_else(|| name.to_owned(), |p| format!("{name}:{p}"))
}

impl Sts {
    /// The request's form, encoded: built and dropped before anything is awaited, since the
    /// encoder is not sent between threads.
    fn form(&self, policy: &str, ttl: Duration) -> String {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("Action", "AssumeRole")
            .append_pair("DurationSeconds", &ttl.as_secs().to_string())
            .append_pair("Policy", policy);
        if let Some(role) = &self.role_arn {
            form.append_pair("RoleArn", role).append_pair("RoleSessionName", "korpis-backup");
        }
        form.append_pair("Version", "2011-06-15").finish()
    }

    /// A credential allowed what `policy` allows, for `ttl`.
    ///
    /// # Errors
    /// The service refused, could not be reached, or answered something unreadable.
    pub async fn assume(&self, policy: &str, ttl: Duration) -> Result<Temporary, StsError> {
        let body = self.form(policy, ttl);
        let date = amz_date(SystemTime::now())?;
        let host = host(&self.endpoint);
        let content_type = "application/x-www-form-urlencoded; charset=utf-8";
        let headers =
            [("content-type", content_type), ("host", host.as_str()), ("x-amz-date", &date)];
        let request = Request {
            method: "POST",
            path: self.endpoint.path(),
            query: "",
            headers: &headers,
            body: body.as_bytes(),
        };
        let signer = Signer {
            access_key_id: &self.access_key_id,
            secret: &self.secret,
            region: &self.region,
            service: "sts",
        };
        let answer = self
            .http
            .post(self.endpoint.clone())
            .header("content-type", content_type)
            .header("x-amz-date", &date)
            .header("authorization", signer.authorization(&request, &date))
            .body(body)
            .send()
            .await
            .map_err(|e| StsError::Failed(e.to_string()))?;
        let status = answer.status();
        let text = answer.text().await.map_err(|e| StsError::Failed(e.to_string()))?;
        if !status.is_success() {
            return Err(StsError::Refused(format!("{status}: {text}")));
        }
        parse(&text)
    }
}

/// The credential an `AssumeRole` answer carries.
fn parse(text: &str) -> Result<Temporary, StsError> {
    let r: AssumeRoleResponse =
        instant_xml::from_str(text).map_err(|e| StsError::Failed(format!("its answer: {e}")))?;
    let c = r.result.credentials;
    let expires: jiff::Timestamp =
        c.expiration.parse().map_err(|e| StsError::Failed(format!("its expiry: {e}")))?;
    Ok(Temporary {
        access_key_id: c.access_key_id,
        secret_access_key: c.secret_access_key,
        session_token: c.session_token,
        expires: SystemTime::from(expires),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_gives_its_credential_and_when_it_ends() {
        let text = r#"<AssumeRoleResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/">
  <AssumeRoleResult>
    <AssumedRoleUser><Arn></Arn><AssumeRoleId></AssumeRoleId></AssumedRoleUser>
    <Credentials>
      <AccessKeyId>Y4RJU1RNFGK48LGO9I2S</AccessKeyId>
      <SecretAccessKey>sYLRKS1Z7hSjluf6gEbb9066hnx315wHTiACPAjg</SecretAccessKey>
      <Expiration>2026-10-14T20:26:12Z</Expiration>
      <SessionToken>eyJhbGciOiJIUzUxMiIsInR5cCI6IkpXVCJ9</SessionToken>
    </Credentials>
  </AssumeRoleResult>
  <ResponseMetadata><RequestId>c6104cbe-af31-11e0-8154-cbc7ccf896c7</RequestId></ResponseMetadata>
</AssumeRoleResponse>"#;
        let t = parse(text).unwrap();
        assert_eq!(t.access_key_id, "Y4RJU1RNFGK48LGO9I2S");
        assert_eq!(t.session_token, "eyJhbGciOiJIUzUxMiIsInR5cCI6IkpXVCJ9");
        let at = jiff::Timestamp::from_second(1_792_009_572).unwrap();
        assert_eq!(t.expires, SystemTime::from(at));
        assert!(!format!("{t:?}").contains("sYLRKS"), "the secret is never shown");
        assert!(parse("<nope/>").is_err());
    }

    #[test]
    fn a_date_is_the_service_s_and_a_host_keeps_a_port_not_its_scheme_s() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_440_938_160);
        assert_eq!(amz_date(at).unwrap(), "20150830T123600Z");
        assert_eq!(host(&"http://127.0.0.1:9000/".parse().unwrap()), "127.0.0.1:9000");
        assert_eq!(host(&"https://sts.amazonaws.com/".parse().unwrap()), "sts.amazonaws.com");
    }
}
