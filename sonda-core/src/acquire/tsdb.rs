//! The HTTP half of TSDB capture: fetch one range query.
//!
//! Deliberately thin — everything decidable without a socket lives in a
//! feature-free sibling. Modelled on [`crate::verify::prometheus`]: one agent,
//! a mandatory timeout, errors that name their URL.
//!
//! Two credential rules. [`Auth`] has no `Display`, no revealing `Debug` and no
//! accessor, so it can only be applied to a request. A `user:pass@` base URL is
//! stored, so only [`scrub_userinfo`]'s output is ever formatted.

use super::response::parse_matrix_response;
use super::FetchedSeries;
use crate::{SondaError, VerifyError};
use std::time::Duration;

/// How to authenticate to the TSDB.
///
/// Protected structurally: a hand-written `Debug`, no `Serialize`, and
/// `#[non_exhaustive]` so an outside caller cannot destructure the secret out
/// without meaning to.
#[derive(Clone, Default)]
#[non_exhaustive]
pub enum Auth {
    /// No credentials.
    #[default]
    None,
    /// `Authorization: Bearer <token>`.
    Bearer(String),
    /// `Authorization: Basic <base64(user:pass)>`, encoded at send time.
    Basic { user: String, password: String },
    /// Arbitrary `Name: value` headers, applied in order.
    Headers(Vec<(String, String)>),
}

impl std::fmt::Debug for Auth {
    /// Prints the *kind* of credential, never the credential.
    ///
    /// A derived Debug here would put the bearer token into any `{:?}` of a
    /// surrounding struct — including error paths nobody audits.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Auth::None => write!(f, "Auth::None"),
            Auth::Bearer(_) => write!(f, "Auth::Bearer(<redacted>)"),
            Auth::Basic { user, .. } => write!(f, "Auth::Basic {{ user: {user:?}, .. }}"),
            Auth::Headers(h) => {
                write!(f, "Auth::Headers([")?;
                for (i, (name, _)) in h.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{name}: <redacted>")?;
                }
                write!(f, "])")
            }
        }
    }
}

/// Encode `user:password` as RFC 4648 base64 for HTTP Basic auth.
///
/// Hand-rolled rather than pulling a dependency for 30 lines; the alphabet and
/// padding are fixed by the RFC and covered by tests.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// The `user:pass@` prefix of a URL's authority, if it has one.
///
/// The last `@` before the path ends the userinfo: RFC 3986 requires an `@`
/// inside it to be percent-encoded, so a later one cannot belong to it.
fn userinfo_of(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("://")?;
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let at = rest[..authority_end].rfind('@')?;
    Some(rest[..=at].to_string())
}

/// Replace a URL credential wherever it appears in `text`.
///
/// Every occurrence, because `ureq` builds its error messages from the URL it
/// was handed and quotes it more than once.
fn scrub_userinfo(text: &str, userinfo: Option<&str>) -> String {
    match userinfo {
        Some(u) => text.replace(u, "<redacted>@"),
        None => text.to_string(),
    }
}

/// A client for one TSDB's range-query endpoint.
pub struct TsdbClient {
    range_url: String,
    /// `range_url` with any credential removed. The only form that is printed.
    display_url: String,
    userinfo: Option<String>,
    agent: ureq::Agent,
    auth: Auth,
}

/// Written by hand so a `user:pass@` base URL cannot reach a log line.
impl std::fmt::Debug for TsdbClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TsdbClient")
            .field("range_url", &self.display_url)
            .field("auth", &self.auth)
            .finish_non_exhaustive()
    }
}

impl TsdbClient {
    /// Build a client for a Prometheus-compatible base URL.
    ///
    /// `timeout` is the overall per-request budget and is mandatory: a capture
    /// against an unreachable endpoint must fail, not hang.
    pub fn new(base_url: &str, auth: Auth, timeout: Duration) -> Self {
        let base = base_url.trim_end_matches('/');
        let range_url = format!("{base}/api/v1/query_range");
        let userinfo = userinfo_of(&range_url);
        TsdbClient {
            display_url: scrub_userinfo(&range_url, userinfo.as_deref()),
            userinfo,
            range_url,
            agent: ureq::AgentBuilder::new().timeout(timeout).build(),
            auth,
        }
    }

    /// Strip the URL credential from anything this client reports.
    fn scrub(&self, text: &str) -> String {
        scrub_userinfo(text, self.userinfo.as_deref())
    }

    /// Run `query` over `start..=end` at `step` and return the series.
    ///
    /// `start` and `end` are unix seconds. The server aligns matrix samples to
    /// the requested grid; [`super::normalize`] is what guarantees the
    /// alignment rather than trusting it.
    pub fn fetch_range(
        &self,
        query: &str,
        start: f64,
        end: f64,
        step: Duration,
    ) -> Result<Vec<FetchedSeries>, SondaError> {
        let mut request = self
            .agent
            .get(&self.range_url)
            .query("query", query)
            .query("start", &format!("{start:.3}"))
            .query("end", &format!("{end:.3}"))
            .query("step", &format!("{:.3}", step.as_secs_f64()));

        request = match &self.auth {
            Auth::None => request,
            Auth::Bearer(token) => request.set("Authorization", &format!("Bearer {token}")),
            Auth::Basic { user, password } => {
                let encoded = base64_encode(format!("{user}:{password}").as_bytes());
                request.set("Authorization", &format!("Basic {encoded}"))
            }
            Auth::Headers(headers) => {
                for (name, value) in headers {
                    request = request.set(name, value);
                }
                request
            }
        };

        let body = request
            .call()
            .map_err(|e| {
                SondaError::Verify(VerifyError::Query {
                    url: self.display_url.clone(),
                    reason: self.scrub(&e.to_string()),
                })
            })?
            .into_string()
            .map_err(|e| {
                SondaError::Verify(VerifyError::BadResponse {
                    url: self.display_url.clone(),
                    reason: self.scrub(&format!("response could not be read: {e}")),
                })
            })?;

        parse_matrix_response(&body).map_err(|reason| {
            SondaError::Verify(VerifyError::BadResponse {
                url: self.display_url.clone(),
                reason: self.scrub(&reason),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"user:pass"), "dXNlcjpwYXNz");
    }

    #[test]
    fn debug_never_prints_a_bearer_token() {
        let a = Auth::Bearer("super-secret-token".to_string());
        let shown = format!("{a:?}");
        assert!(
            !shown.contains("super-secret-token"),
            "token leaked into Debug: {shown}"
        );
        assert!(shown.contains("redacted"), "{shown}");
    }

    #[test]
    fn debug_never_prints_a_basic_password_or_a_header_value() {
        let a = Auth::Basic {
            user: "alice".to_string(),
            password: "hunter2".to_string(),
        };
        let shown = format!("{a:?}");
        assert!(!shown.contains("hunter2"), "password leaked: {shown}");
        assert!(shown.contains("alice"), "user is not a secret: {shown}");

        let h = Auth::Headers(vec![("X-Scope-OrgID".into(), "tenant-secret".into())]);
        let shown = format!("{h:?}");
        assert!(!shown.contains("tenant-secret"), "header leaked: {shown}");
        assert!(shown.contains("X-Scope-OrgID"), "{shown}");
    }

    #[test]
    fn debug_of_the_client_does_not_carry_the_credential() {
        let c = TsdbClient::new(
            "http://localhost:9090",
            Auth::Bearer("leak-me".to_string()),
            Duration::from_secs(5),
        );
        let shown = format!("{c:?}");
        assert!(!shown.contains("leak-me"), "client Debug leaked: {shown}");
    }

    #[test]
    fn base_url_trailing_slashes_do_not_double_up() {
        let c = TsdbClient::new("http://x:9090/", Auth::None, Duration::from_secs(1));
        assert_eq!(c.range_url, "http://x:9090/api/v1/query_range");
    }

    #[test]
    fn userinfo_is_found_only_in_the_authority() {
        assert_eq!(
            userinfo_of("http://admin:s3cret@host:9090/api"),
            Some("admin:s3cret@".to_string())
        );
        assert_eq!(userinfo_of("http://tok@host/api"), Some("tok@".to_string()));
        assert_eq!(userinfo_of("http://host:9090/api"), None);
        // An `@` in the path is not a credential.
        assert_eq!(userinfo_of("http://host:9090/api/a@b"), None);
        assert_eq!(userinfo_of("http://host:9090/?q=a@b"), None);
        assert_eq!(userinfo_of("not-a-url"), None);
    }

    #[test]
    fn a_url_credential_is_kept_for_the_wire_and_stripped_from_everything_else() {
        let c = TsdbClient::new(
            "http://admin:urlsecret@127.0.0.1:9090",
            Auth::None,
            Duration::from_secs(1),
        );
        assert!(
            c.range_url.contains("admin:urlsecret@"),
            "the request itself still needs the credential: {}",
            c.range_url
        );
        assert_eq!(
            c.display_url,
            "http://<redacted>@127.0.0.1:9090/api/v1/query_range"
        );

        let shown = format!("{c:?}");
        assert!(!shown.contains("urlsecret"), "client Debug leaked: {shown}");

        // ureq quotes the URL more than once in one message.
        let doubled = format!("{0} failed: {0}", c.range_url);
        assert!(!c.scrub(&doubled).contains("urlsecret"), "{doubled}");
    }

    #[test]
    fn a_url_without_a_credential_is_reported_verbatim() {
        let c = TsdbClient::new("http://127.0.0.1:9090", Auth::None, Duration::from_secs(1));
        assert_eq!(c.display_url, c.range_url);
        assert_eq!(c.scrub("nothing to strip"), "nothing to strip");
    }
}
