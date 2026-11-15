//! Artifact acquisition adapter surface.
//!
//! Network details terminate here; workflows consume bounded response metadata
//! and a streaming body rather than HTTP or GPUI types.

use std::{
    error::Error,
    fmt,
    io::{Read, Take},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use zed_reqwest::{
    Url,
    blocking::Client,
    header::{ACCEPT_ENCODING, CONTENT_RANGE, RANGE, RETRY_AFTER},
    redirect,
};

use crate::{
    manifest::{ManifestResponse, Pd2ClientBase},
    operation::{OperationControl, WorkPhase},
};

const REQUEST_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(15);
/// Redirect hops a user-supplied PD2 client base may take.
const CUSTOM_REDIRECT_LIMIT: usize = 10;
static RETRY_JITTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug)]
pub(crate) struct RetryPolicy {
    max_attempts: usize,
    max_elapsed: Duration,
    base_delay: Duration,
    max_delay: Duration,
}

impl RetryPolicy {
    pub(crate) const fn standard() -> Self {
        Self {
            max_attempts: 5,
            max_elapsed: Duration::from_secs(45),
            base_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(5),
        }
    }

    /// One attempt and no retry, for a fetch whose failure only keeps what
    /// is there.
    pub(crate) const fn once() -> Self {
        Self {
            max_attempts: 1,
            max_elapsed: Duration::ZERO,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        }
    }

    #[cfg(test)]
    pub(crate) const fn immediate_for_tests() -> Self {
        Self {
            max_attempts: 5,
            max_elapsed: Duration::from_secs(1),
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        }
    }

    pub(crate) const fn max_attempts(self) -> usize {
        self.max_attempts
    }

    pub(crate) fn delay(
        self,
        failed_attempt: usize,
        retry_after: Option<Duration>,
        jitter_sample: u64,
    ) -> Duration {
        let multiplier = 1_u32 << failed_attempt.min(8);
        let exponential = self
            .base_delay
            .checked_mul(multiplier)
            .unwrap_or(self.max_delay)
            .min(self.max_delay);
        let millis = exponential.as_millis().min(u128::from(u64::MAX)) as u64;
        let window = millis / 5;
        let jittered_millis = if window == 0 {
            millis
        } else {
            millis
                .saturating_sub(window)
                .saturating_add(jitter_sample % (window.saturating_mul(2).saturating_add(1)))
        };
        let jittered = Duration::from_millis(jittered_millis).min(self.max_delay);
        retry_after
            .map(|delay| delay.min(self.max_delay))
            .map_or(jittered, |delay| delay.max(jittered))
    }

    pub(crate) fn pause(
        self,
        failed_attempt: usize,
        retry_after: Option<Duration>,
        started: Instant,
        control: Option<&OperationControl>,
    ) -> bool {
        if failed_attempt + 1 >= self.max_attempts
            || started.elapsed() >= self.max_elapsed
            || control.is_some_and(OperationControl::is_cancelled)
        {
            return false;
        }
        let remaining = self.max_elapsed.saturating_sub(started.elapsed());
        let sample = RETRY_JITTER.fetch_add(1, Ordering::Relaxed);
        let mut delay = self
            .delay(failed_attempt, retry_after, sample)
            .min(remaining);
        while !delay.is_zero() {
            if control.is_some_and(OperationControl::is_cancelled) {
                return false;
            }
            let slice = delay.min(Duration::from_millis(25));
            thread::sleep(slice);
            delay = delay.saturating_sub(slice);
        }
        !control.is_some_and(OperationControl::is_cancelled)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ObjectRequest {
    pub(crate) url: String,
    pub(crate) range_start: Option<u64>,
    /// Only objects from a custom PD2 client base may end at a redirected
    /// URL; their content is still checked against the manifest MD5.
    pub(crate) accept_redirects: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ContentRange {
    pub(crate) start: u64,
    pub(crate) end_inclusive: u64,
    pub(crate) complete_length: u64,
}

pub(crate) struct ObjectResponse {
    pub(crate) final_url: String,
    pub(crate) status: u16,
    pub(crate) declared_length: Option<u64>,
    pub(crate) content_range: Option<ContentRange>,
    pub(crate) retry_after: Option<Duration>,
    pub(crate) body: Box<dyn Read + Send>,
}

/// Approved-source boundary. A concrete HTTP adapter owns TLS, timeout, and
/// redirect mechanics; the workflow still revalidates every returned origin.
pub(crate) trait Pd2Source: Send {
    /// The base whose manifest this source serves.
    fn client_base(&self) -> Pd2ClientBase {
        Pd2ClientBase::BuiltIn
    }
    /// The phase an update reports while it gets this source's manifest.
    /// A source that reads its manifest from the installed files, with no
    /// download, names a check instead.
    fn manifest_phase(&self) -> WorkPhase {
        WorkPhase::ClientManifest
    }
    fn fetch_manifest(&mut self) -> Result<ManifestResponse, NetworkError>;
    fn fetch_manifest_controlled(
        &mut self,
        _control: &OperationControl,
    ) -> Result<ManifestResponse, NetworkError> {
        self.fetch_manifest()
    }
    fn open_object(&mut self, request: &ObjectRequest) -> Result<ObjectResponse, NetworkError>;
    /// A local copy of the object at `url` whose MD5 is the one the
    /// manifest being applied names, when this source holds one (the
    /// Download step's cache). The updater copies it instead of opening
    /// a response, and verifies the copy's MD5 again.
    fn local_object(&mut self, _url: &str) -> Option<std::fs::File> {
        None
    }
    fn open_object_controlled(
        &mut self,
        request: &ObjectRequest,
        control: &OperationControl,
    ) -> Result<ObjectResponse, NetworkError> {
        if control.is_cancelled() {
            return Err(NetworkError::transient("object request was cancelled"));
        }
        let response = self.open_object(request)?;
        if control.is_cancelled() {
            Err(NetworkError::transient("object request was cancelled"))
        } else {
            Ok(response)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NetworkErrorKind {
    Transient,
    Permanent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NetworkError {
    kind: NetworkErrorKind,
    message: &'static str,
    retry_after: Option<Duration>,
}

impl NetworkError {
    pub(crate) fn transient(message: &'static str) -> Self {
        Self {
            kind: NetworkErrorKind::Transient,
            message,
            retry_after: None,
        }
    }

    pub(crate) fn permanent(message: &'static str) -> Self {
        Self {
            kind: NetworkErrorKind::Permanent,
            message,
            retry_after: None,
        }
    }

    fn transient_with_retry(message: &'static str, retry_after: Option<Duration>) -> Self {
        Self {
            kind: NetworkErrorKind::Transient,
            message,
            retry_after,
        }
    }

    pub(crate) fn is_transient(&self) -> bool {
        self.kind == NetworkErrorKind::Transient
    }

    pub(crate) const fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }
}

impl fmt::Display for NetworkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl Error for NetworkError {}

/// HTTP adapter. For the built-in base, redirects are followed only
/// within the frozen origin and the workflow separately verifies the final
/// canonical URL. A custom base follows bounded `http`/`https`
/// redirects anywhere; its objects are still verified against the manifest.
pub(crate) struct SystemPd2Source {
    client: Client,
    base: Pd2ClientBase,
}

impl SystemPd2Source {
    pub(crate) fn new() -> Result<Self, NetworkError> {
        Self::with_client_base(Pd2ClientBase::BuiltIn)
    }

    pub(crate) fn with_client_base(base: Pd2ClientBase) -> Result<Self, NetworkError> {
        let custom = base.is_custom();
        let redirect = redirect::Policy::custom(move |attempt| {
            let allowed = if custom {
                attempt.previous().len() < CUSTOM_REDIRECT_LIMIT
                    && matches!(attempt.url().scheme(), "http" | "https")
            } else {
                approved_origin(attempt.url())
            };
            if allowed {
                attempt.follow()
            } else {
                attempt.error("PD2 redirect left the approved origin")
            }
        });
        let client = Client::builder()
            .user_agent(concat!(
                "pd2-launcher/",
                env!("CARGO_PKG_VERSION"),
                " linux"
            ))
            .connect_timeout(Duration::from_secs(10))
            // A request attempt includes connect, headers, and body. Large
            // objects resume through verified Range attempts, so no single
            // stalled transport operation can pin a mutator indefinitely.
            .timeout(REQUEST_ATTEMPT_TIMEOUT)
            .redirect(redirect)
            .build()
            .map_err(|_| NetworkError::permanent("approved HTTP client could not initialize"))?;
        Ok(Self { client, base })
    }

    fn object_url_allowed(&self, value: &str) -> bool {
        match &self.base {
            Pd2ClientBase::BuiltIn => approved_url(value),
            Pd2ClientBase::Custom(base) => value
                .strip_prefix(base.as_str())
                .is_some_and(|rest| rest.starts_with('/')),
        }
    }
}

impl Pd2Source for SystemPd2Source {
    fn client_base(&self) -> Pd2ClientBase {
        self.base.clone()
    }

    fn fetch_manifest(&mut self) -> Result<ManifestResponse, NetworkError> {
        self.fetch_manifest_response(None)
    }

    fn fetch_manifest_controlled(
        &mut self,
        control: &OperationControl,
    ) -> Result<ManifestResponse, NetworkError> {
        self.fetch_manifest_response(Some(control))
    }

    fn open_object(&mut self, request: &ObjectRequest) -> Result<ObjectResponse, NetworkError> {
        if !self.object_url_allowed(&request.url) {
            return Err(NetworkError::permanent("object URL was not approved"));
        }
        let mut builder = self
            .client
            .get(&request.url)
            .header(ACCEPT_ENCODING, "identity");
        if let Some(start) = request.range_start {
            builder = builder.header(RANGE, format!("bytes={start}-"));
        }
        let response = builder.send().map_err(|error| {
            crate::activity::record_download(
                "pd2-client-file",
                &request.url,
                None,
                crate::activity::transport_error_kind(&error),
            );
            transport_error(error)
        })?;
        let final_url = response.url().to_string();
        let status = response.status().as_u16();
        if !matches!(status, 200 | 206) {
            crate::activity::record_download(
                "pd2-client-file",
                &request.url,
                Some(status),
                "status",
            );
        }
        let retry_after = parse_retry_after(response.headers().get(RETRY_AFTER));
        let declared_length = response.content_length();
        let content_range = response
            .headers()
            .get(CONTENT_RANGE)
            .map(|value| {
                value
                    .to_str()
                    .map_err(|_| NetworkError::permanent("Content-Range was not ASCII"))
                    .and_then(parse_content_range)
            })
            .transpose()?;
        Ok(ObjectResponse {
            final_url,
            status,
            declared_length,
            content_range,
            retry_after,
            body: Box::new(response),
        })
    }
}

impl SystemPd2Source {
    fn fetch_manifest_response(
        &mut self,
        control: Option<&OperationControl>,
    ) -> Result<ManifestResponse, NetworkError> {
        let manifest_url = self.base.manifest_url();
        let response = self
            .client
            .get(&manifest_url)
            .header(ACCEPT_ENCODING, "identity")
            .send()
            .map_err(|error| {
                crate::activity::record_download(
                    "pd2-manifest",
                    &manifest_url,
                    None,
                    crate::activity::transport_error_kind(&error),
                );
                transport_error(error)
            })?;
        let status = response.status().as_u16();
        let retry_after = parse_retry_after(response.headers().get(RETRY_AFTER));
        if status != 200 {
            crate::activity::record_download("pd2-manifest", &manifest_url, Some(status), "status");
            return Err(status_error(status, retry_after));
        }
        let final_url = response.url().to_string();
        let declared_length = response.content_length();
        let mut body = Vec::new();
        let mut bounded: Take<_> = response.take(1024 * 1024 + 1);
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            if control.is_some_and(OperationControl::is_cancelled) {
                return Err(NetworkError::transient("manifest request was cancelled"));
            }
            let read = bounded
                .read(&mut buffer)
                .map_err(|_| NetworkError::transient("manifest response was interrupted"))?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&buffer[..read]);
        }
        Ok(ManifestResponse {
            requested_url: manifest_url,
            final_url,
            declared_length,
            body,
        })
    }
}

fn approved_url(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| approved_origin(&url))
}

fn approved_origin(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("pd2-client-files.projectdiablo2.com")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
}

pub(crate) fn parse_content_range(value: &str) -> Result<ContentRange, NetworkError> {
    let value = value
        .strip_prefix("bytes ")
        .ok_or_else(|| NetworkError::permanent("Content-Range unit was invalid"))?;
    let (range, complete) = value
        .split_once('/')
        .ok_or_else(|| NetworkError::permanent("Content-Range total was missing"))?;
    let (start, end) = range
        .split_once('-')
        .ok_or_else(|| NetworkError::permanent("Content-Range bounds were missing"))?;
    let start = start
        .parse()
        .map_err(|_| NetworkError::permanent("Content-Range start was invalid"))?;
    let end_inclusive = end
        .parse()
        .map_err(|_| NetworkError::permanent("Content-Range end was invalid"))?;
    let complete_length = complete
        .parse()
        .map_err(|_| NetworkError::permanent("Content-Range total was invalid"))?;
    Ok(ContentRange {
        start,
        end_inclusive,
        complete_length,
    })
}

fn transport_error(_error: zed_reqwest::Error) -> NetworkError {
    NetworkError::transient("PD2 response transport failed")
}

fn status_error(status: u16, retry_after: Option<Duration>) -> NetworkError {
    if status == 429 || (500..=599).contains(&status) {
        NetworkError::transient_with_retry("PD2 service returned a retryable status", retry_after)
    } else {
        NetworkError::permanent("PD2 service returned a rejected status")
    }
}

pub(crate) fn parse_retry_after(
    value: Option<&zed_reqwest::header::HeaderValue>,
) -> Option<Duration> {
    value?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approved_origin_is_exact_and_content_ranges_are_strict() {
        assert!(approved_url(
            "https://pd2-client-files.projectdiablo2.com/Game.exe"
        ));
        for rejected in [
            "http://pd2-client-files.projectdiablo2.com/Game.exe",
            "https://pd2-client-files.projectdiablo2.com.example/Game.exe",
            "https://user@pd2-client-files.projectdiablo2.com/Game.exe",
            "https://pd2-client-files.projectdiablo2.com:444/Game.exe",
        ] {
            assert!(!approved_url(rejected), "{rejected}");
        }
        assert_eq!(
            parse_content_range("bytes 3-9/10").unwrap(),
            ContentRange {
                start: 3,
                end_inclusive: 9,
                complete_length: 10,
            }
        );
        assert!(parse_content_range("items 3-9/10").is_err());
        assert!(parse_content_range("bytes 3-9/*").is_err());
    }

    #[test]
    fn a_custom_client_base_bounds_objects_to_that_base_only() {
        let source = SystemPd2Source::with_client_base(Pd2ClientBase::Custom(
            "https://mirror.example/pd2".to_owned(),
        ))
        .unwrap();
        assert!(source.object_url_allowed("https://mirror.example/pd2/Game.exe"));
        assert!(!source.object_url_allowed("https://mirror.example/pd2x/Game.exe"));
        assert!(!source.object_url_allowed("https://pd2-client-files.projectdiablo2.com/Game.exe"));
        assert_eq!(
            source.client_base().manifest_url(),
            "https://mirror.example/pd2/metadata.json"
        );
        let builtin = SystemPd2Source::new().unwrap();
        assert!(builtin.object_url_allowed("https://pd2-client-files.projectdiablo2.com/Game.exe"));
        assert!(!builtin.object_url_allowed("https://mirror.example/pd2/Game.exe"));
        assert_eq!(builtin.client_base(), Pd2ClientBase::BuiltIn);
    }

    #[test]
    fn retry_policy_is_small_jittered_and_caps_retry_after() {
        let policy = RetryPolicy::standard();
        assert_eq!(policy.max_attempts(), 5);
        let low = policy.delay(2, None, 0);
        let high = policy.delay(2, None, u64::MAX);
        assert!(low < high);
        assert!(high <= Duration::from_secs(5));
        assert_eq!(
            policy.delay(0, Some(Duration::from_secs(600)), 0),
            Duration::from_secs(5)
        );

        let control = OperationControl::default();
        control.request_cancel();
        assert!(!policy.pause(
            0,
            Some(Duration::from_secs(600)),
            Instant::now(),
            Some(&control)
        ));

        let once = RetryPolicy::once();
        assert_eq!(once.max_attempts(), 1);
        assert!(!once.pause(0, None, Instant::now(), None));
    }
}
