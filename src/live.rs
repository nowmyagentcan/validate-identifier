//! The registries' HTTP client: the OS trust store, proxies from the environment, base URL
//! overrides, pacing per source, a 1 MiB answer cap and no redirects; and DNS lookups.
//! `--debug` lines go to stderr with every secret replaced by REDACTED.

use std::collections::HashMap;
use std::io::Write;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hickory_resolver::proto::rr::RecordType;
use ureq::Agent;
use ureq::http::Request;
use ureq::tls::{RootCerts, TlsConfig};

use crate::check::usage;
use crate::item::{Error, Live, LiveStatus};
use crate::source::dns::{Dns, Records};

/// One registry: its name in results, its base URL override, its default base and the least
/// time between two request starts (spec §7.2).
pub struct Source {
    pub name: &'static str,
    pub var: &'static str,
    pub base: &'static str,
    pub gap: Duration,
}

pub const USER_AGENT: &str = concat!(
    "validate-identifier/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/nowmyagentcan/validate-identifier)"
);

/// The largest answer read; a larger one is `unavailable`.
const LIMIT: u64 = 1 << 20;

static DEBUG: AtomicBool = AtomicBool::new(false);
static SECRETS: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn set_debug(on: bool) {
    DEBUG.store(on, Ordering::Relaxed);
}

/// Keeps `s` out of every `--debug` line.
pub fn secret(s: &str) {
    if !s.is_empty() {
        SECRETS.lock().unwrap().push(s.into());
    }
}

/// A `--debug` line on stderr, secrets redacted; a closed stderr loses it quietly.
pub fn debug(line: impl Into<String>) {
    if DEBUG.load(Ordering::Relaxed) {
        let mut line = line.into();
        for s in SECRETS.lock().unwrap().iter() {
            line = line.replace(s.as_str(), "REDACTED");
        }
        let _ = writeln!(anstream::stderr(), "debug: {line}");
    }
}

/// An HTTP answer, any status.
#[derive(Default)]
pub struct Reply {
    pub status: u16,
    pub body: String,
    /// `Retry-After` in seconds.
    pub retry_after: Option<u64>,
    /// Where a redirect points.
    pub location: Option<String>,
}

pub struct Client {
    /// `--requester` as prefix and number, for the sources that return consultation numbers.
    pub requester: Option<(String, String)>,
    /// HMRC's bearer token, once fetched.
    pub token: Option<String>,
    /// `--dns`: the DNS server to ask instead of the system's.
    pub dns: Option<SocketAddr>,
    resolver: Option<Result<Dns, String>>,
    agent: Agent,
    bases: HashMap<&'static str, String>,
    last: HashMap<&'static str, Instant>,
    start: Instant,
}

impl Client {
    /// A client for `sources`; a bad base URL override is a usage error.
    pub fn new(sources: &[&Source]) -> Result<Self, Error> {
        let bases = sources
            .iter()
            .filter(|s| !s.var.is_empty())
            .map(|s| Ok((s.name, base(s, std::env::var(s.var).ok())?)))
            .collect::<Result<_, Error>>()?;
        let ring = Arc::new(rustls::crypto::ring::default_provider());
        let tls = TlsConfig::builder()
            .root_certs(RootCerts::PlatformVerifier)
            .unversioned_rustls_crypto_provider(ring)
            .build();
        let agent = Agent::config_builder()
            .tls_config(tls)
            .user_agent(USER_AGENT)
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_global(Some(Duration::from_secs(20)))
            .build()
            .into();
        Ok(Client {
            requester: None,
            token: None,
            dns: None,
            resolver: None,
            agent,
            bases,
            last: HashMap::new(),
            start: Instant::now(),
        })
    }

    /// `path` at `s`'s base URL.
    pub fn url(&self, s: &Source, path: &str) -> String {
        format!("{}{path}", self.bases[s.name])
    }

    /// `name`'s DNS records of type `t`, from a resolver made on first use.
    pub fn lookup(&mut self, name: &str, t: RecordType) -> Result<Records, String> {
        let since = self.start.elapsed().as_secs_f64();
        debug(format!("DNS {t} {name} (+{since:.3} s)"));
        let at = Instant::now();
        let server = self.dns;
        let answer = match self.resolver.get_or_insert_with(|| Dns::new(server)) {
            Ok(dns) => dns.lookup(name, t),
            Err(why) => Err(why.clone()),
        };
        let ms = at.elapsed().as_millis();
        match &answer {
            Ok(Records::Some(r)) => debug(format!("DNS {} records in {ms} ms", r.len())),
            Ok(Records::None) => debug(format!("DNS no {t} records in {ms} ms")),
            Ok(Records::NxDomain) => debug(format!("DNS NXDOMAIN in {ms} ms")),
            Err(why) => debug(format!("DNS failed in {ms} ms: {why}")),
        }
        answer
    }

    /// Sends one request once `s`'s gap since its last request start has passed; a failure is
    /// worded for people.
    #[expect(clippy::result_large_err, reason = "one per paced request")]
    pub fn send(&mut self, s: &Source, req: Request<Vec<u8>>) -> Result<Reply, Live> {
        let host = req.uri().host().unwrap_or_default().to_string();
        self.fetch(s, req).map_err(|e| {
            let proxy = self.agent.config().proxy().map(|p| p.host().to_string());
            failure(s, &host, proxy.as_deref(), e)
        })
    }

    /// `send`, with a failure as the transport's own error.
    pub fn fetch(&mut self, s: &Source, req: Request<Vec<u8>>) -> Result<Reply, ureq::Error> {
        if let Some(last) = self.last.get(s.name) {
            std::thread::sleep(s.gap.saturating_sub(last.elapsed()));
        }
        let at = Instant::now();
        self.last.insert(s.name, at);
        let since = (at - self.start).as_secs_f64();
        debug(format!(
            "{} {} {} (+{since:.3} s)",
            s.name,
            req.method(),
            req.uri()
        ));
        let answer = self.agent.run(req).and_then(|mut r| {
            let body = r.body_mut().with_config().limit(LIMIT).read_to_string()?;
            Ok((r, body))
        });
        let ms = at.elapsed().as_millis();
        match answer {
            Ok((r, body)) => {
                let header = |name| r.headers().get(name).and_then(|v| v.to_str().ok());
                let kind = header("content-type").unwrap_or("no content type");
                let n = body.len();
                debug(format!(
                    "{} HTTP {} {kind}, {n} bytes in {ms} ms",
                    s.name,
                    r.status()
                ));
                Ok(Reply {
                    status: r.status().as_u16(),
                    retry_after: header("retry-after").and_then(|v| v.trim().parse().ok()),
                    location: header("location").map(String::from),
                    body,
                })
            }
            Err(e) => {
                debug(format!("{} failed in {ms} ms: {e}", s.name));
                Err(e)
            }
        }
    }
}

impl Client {
    /// Sends a request built by `req` and reads the answer with `read`; a transient failure
    /// is tried again (after 2 s, then 4 s, or the 429's `Retry-After`), never a timeout.
    pub fn ask(
        &mut self,
        s: &Source,
        req: impl Fn() -> Request<Vec<u8>>,
        read: impl Fn(&Reply) -> Result<Live, Error>,
    ) -> Result<Live, Error> {
        let mut tries = 0;
        loop {
            let live = match self.send(s, req()) {
                Ok(reply) => read(&reply)?,
                Err(failed) => failed,
            };
            if tries >= live.retries {
                return Ok(live);
            }
            let wait = match live.code.as_deref() {
                Some("429") => live.retry_after.unwrap_or(2).max(2),
                _ => 2 << tries,
            };
            debug(format!("{} again in {wait} s", s.name));
            std::thread::sleep(Duration::from_secs(wait));
            tries += 1;
        }
    }
}

/// Percent-encoding for query and form values (RFC 3986 unreserved characters kept).
pub fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `s`'s base URL: its override `url` when set, which must be https://, or http:// to this
/// computer.
fn base(s: &Source, url: Option<String>) -> Result<String, Error> {
    let Some(url) = url.filter(|v| !v.is_empty()) else {
        return Ok(s.base.into());
    };
    let url = url.trim_end_matches('/');
    let https = url.len() > "https://".len() && url.starts_with("https://");
    if !https && !local_http(url) {
        let message = format!(
            "{} must start with https:// (http:// only for 127.0.0.1, [::1] or localhost).",
            s.var
        );
        return Err(usage(
            message,
            "Fix it, or unset it to use the official address.",
        ));
    }
    Ok(url.into())
}

/// Whether `url` is plain http:// to this computer.
pub fn local_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or_default();
    let port = |(_, p): &(&str, &str)| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    let host = host.rsplit_once(':').filter(port).map_or(host, |(h, _)| h);
    ["127.0.0.1", "[::1]", "localhost"].contains(&host)
}

/// `unavailable` from `s`, with what to tell people.
pub fn unavailable(s: &Source, message: impl Into<String>) -> Live {
    Live::new(s.name, LiveStatus::Unavailable, message)
}

/// `unavailable` for an answer that isn't in the source's documented shape.
pub fn unreadable(s: &Source, body: &str) -> Live {
    let head: String = body.chars().take(500).collect();
    debug(format!("{} answer we couldn't read: {head}", s.name));
    unavailable(s, format!("{} sent a response we couldn't read.", s.name))
}

/// What a request that got no HTTP answer means, re-worded so no URL or secret leaks.
fn failure(s: &Source, host: &str, proxy: Option<&str>, e: ureq::Error) -> Live {
    use ureq::Error as E;
    use ureq::Timeout as T;
    let reach = |why: &str, retries| Live {
        retries,
        ..unavailable(
            s,
            format!("Couldn't reach {host}: {why}. This is not a verdict on the number."),
        )
    };
    match e {
        E::Timeout(T::Connect | T::Resolve) => reach("no answer within 5 s", 0),
        E::Timeout(_) => unavailable(s, format!("{} took longer than 20 s.", s.name)),
        E::HostNotFound => reach("host not found", 0),
        E::Io(e) => reach(&e.kind().to_string(), 1),
        E::ConnectionFailed => reach("connection failed", 1),
        E::ConnectProxyFailed(_) | E::InvalidProxyUrl => reach(
            &format!("proxy error at {}", proxy.unwrap_or("the proxy")),
            1,
        ),
        E::Tls(why) => certificate(s, host, why),
        E::Rustls(why) => certificate(s, host, &why.to_string()),
        E::BodyExceedsLimit(_) => {
            unavailable(s, format!("{} sent more data than expected.", s.name))
        }
        _ => unreadable(s, ""),
    }
}

fn certificate(s: &Source, host: &str, why: &str) -> Live {
    unavailable(s, format!("Couldn't verify {host}'s certificate: {why}."))
}

impl Reply {
    /// What an HTTP status the source doesn't document means: a 429 is `rate_limited`,
    /// anything else `unavailable`; 5xx and short 429 waits may be tried again.
    pub fn failed(&self, s: &Source) -> Live {
        let code = Some(self.status.to_string());
        if self.status == 429 {
            let wait = match self.retry_after {
                Some(n) => format!("Try again in {n} s."),
                None => "Try again in a minute.".into(),
            };
            let message = format!("{} asked us to slow down (429). {wait}", s.name);
            return Live {
                code,
                retry_after: self.retry_after,
                retries: u32::from(self.retry_after.is_none_or(|n| n <= 30)),
                ..Live::new(s.name, LiveStatus::RateLimited, message)
            };
        }
        // The European Commission sends visitors to sorry.ec.europa.eu during maintenance.
        let sorry = self
            .location
            .as_deref()
            .is_some_and(|l| l.contains("//sorry."));
        let what = match self.status {
            300..=399 if sorry => "is down for maintenance",
            300..=399 => "redirected the request",
            _ => "answered",
        };
        Live {
            code,
            retries: u32::from(self.status >= 500),
            ..unavailable(
                s,
                format!(
                    "{} {what} (HTTP {}). This is not a verdict on the number.",
                    s.name, self.status
                ),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST: Source = Source {
        name: "Test",
        var: "VALIDATE_IDENTIFIER_TEST_URL",
        base: "https://example.org",
        gap: Duration::from_millis(50),
    };

    #[test]
    fn overrides_need_https_unless_they_stay_on_this_computer() {
        let with = |url: &str| base(&TEST, Some(url.into()));
        for ok in [
            "http://127.0.0.1:8080",
            "http://[::1]",
            "http://localhost:1/",
            "https://mirror.example.org/vies",
        ] {
            assert_eq!(with(ok).unwrap(), ok.trim_end_matches('/'), "{ok}");
        }
        for bad in [
            "http://example.org",
            "http://127.0.0.1.example.org",
            "http://127.0.0.1@example.org",
            "ftp://127.0.0.1",
            "https://",
            "example.org",
        ] {
            let e = with(bad).unwrap_err();
            assert_eq!(e.code, "usage", "{bad}");
            assert!(
                e.message
                    .starts_with("VALIDATE_IDENTIFIER_TEST_URL must start with https://")
            );
        }
        assert_eq!(with("").unwrap(), "https://example.org");
    }

    #[test]
    fn values_are_percent_encoded() {
        assert_eq!(encode("a b&c=d+é~"), "a%20b%26c%3Dd%2B%C3%A9~");
    }

    #[test]
    fn undocumented_statuses_never_become_a_verdict() {
        let reply = |status, retry_after| Reply {
            status,
            retry_after,
            ..Reply::default()
        };
        let busy = reply(429, Some(30)).failed(&TEST);
        assert_eq!(busy.status, LiveStatus::RateLimited);
        assert_eq!(
            busy.message,
            "Test asked us to slow down (429). Try again in 30 s."
        );
        assert_eq!((busy.retry_after, busy.retries), (Some(30), 1));
        assert_eq!(reply(429, Some(31)).failed(&TEST).retries, 0);
        assert_eq!(
            reply(429, None).failed(&TEST).message,
            "Test asked us to slow down (429). Try again in a minute."
        );
        let down = reply(503, None).failed(&TEST);
        assert_eq!((down.status, down.retries), (LiveStatus::Unavailable, 1));
        assert_eq!(down.code.as_deref(), Some("503"));
        assert_eq!(
            reply(302, None).failed(&TEST).message,
            "Test redirected the request (HTTP 302). This is not a verdict on the number."
        );
        assert_eq!(reply(400, None).failed(&TEST).retries, 0);
    }

    #[test]
    fn transport_failures_name_the_host_and_retry_only_when_it_may_help() {
        let f = |e| failure(&TEST, "ec.europa.eu", Some("proxy.example"), e);
        let refused = f(ureq::Error::Io(
            std::io::ErrorKind::ConnectionRefused.into(),
        ));
        assert_eq!(
            refused.message,
            "Couldn't reach ec.europa.eu: connection refused. This is not a verdict on the number."
        );
        assert_eq!(
            (refused.status, refused.retries),
            (LiveStatus::Unavailable, 1)
        );
        let slow = f(ureq::Error::Timeout(ureq::Timeout::Global));
        assert_eq!(
            (slow.message.as_str(), slow.retries),
            ("Test took longer than 20 s.", 0)
        );
        assert_eq!(f(ureq::Error::HostNotFound).retries, 0);
        assert!(
            f(ureq::Error::ConnectProxyFailed("x".into()))
                .message
                .contains("proxy error at proxy.example")
        );
        assert_eq!(
            f(ureq::Error::Tls("bad")).message,
            "Couldn't verify ec.europa.eu's certificate: bad."
        );
        assert_eq!(
            f(ureq::Error::BodyExceedsLimit(LIMIT)).message,
            "Test sent more data than expected."
        );
    }
}
