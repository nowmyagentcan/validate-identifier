//! The registries' HTTP client: the OS trust store, proxies from the environment, base URL
//! overrides, pacing per source, a 1 MiB answer cap and no redirects; and DNS lookups.
//! `--debug` lines go to stderr with every secret replaced by REDACTED.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flate2::read::MultiGzDecoder;
use hickory_resolver::proto::rr::RecordType;
use serde_json::Value;
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
/// Each source's last request start in this process, so pacing holds across the runs of an
/// interactive session, the page and the MCP server too.
static LAST: Mutex<BTreeMap<&str, Instant>> = Mutex::new(BTreeMap::new());

pub fn set_debug(on: bool) {
    DEBUG.store(on, Ordering::Relaxed);
}

/// Keeps `s`, as given and as sent in a form or query, out of every `--debug` line and result.
pub fn secret(s: &str) {
    let mut secrets = SECRETS.lock().unwrap();
    for s in [s.to_string(), encode(s)] {
        if !s.is_empty() && !secrets.contains(&s) {
            secrets.push(s);
        }
    }
}

/// `text` with every secret replaced by REDACTED, in any letter case: a registry may send
/// the sent form back with its hex digits in lower case.
fn redact(text: &str) -> String {
    let mut text = text.to_string();
    for s in SECRETS.lock().unwrap().iter() {
        // ASCII case changes keep every byte where it is, so offsets carry over.
        let lower = text.to_ascii_lowercase();
        let (mut out, mut last) = (String::new(), 0);
        for (at, _) in lower.match_indices(&s.to_ascii_lowercase()) {
            out += &text[last..at];
            out += "REDACTED";
            last = at + s.len();
        }
        text = out + &text[last..];
    }
    text
}

/// `live` with every secret redacted: a registry's answer may echo what it was sent.
pub fn scrub(mut live: Live) -> Live {
    fn strings(v: &mut Value) {
        match v {
            Value::String(s) => *s = redact(s),
            Value::Array(a) => a.iter_mut().for_each(strings),
            Value::Object(o) => o.values_mut().for_each(strings),
            _ => {}
        }
    }
    live.code = live.code.as_deref().map(redact);
    live.message = redact(&live.message);
    strings(&mut live.data);
    live
}

/// A `--debug` line on stderr, secrets redacted; a closed stderr loses it quietly.
pub fn debug(line: impl Into<String>) {
    if DEBUG.load(Ordering::Relaxed) {
        let _ = writeln!(anstream::stderr(), "debug: {}", redact(&line.into()));
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
    /// HTTP requests in any source that failed in a row for want of DNS or a route, and which.
    offline: (u32, &'static str),
    agent: Agent,
    bases: HashMap<&'static str, String>,
    start: Instant,
}

impl Client {
    /// A client for `sources`; a bad base URL override is a usage error.
    pub fn new(sources: &[&Source]) -> Result<Self, Error> {
        for var in ["HMRC_CLIENT_ID", "HMRC_CLIENT_SECRET", "ABN_LOOKUP_GUID"] {
            secret(&std::env::var(var).unwrap_or_default());
        }
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
            .timeout_resolve(Some(Duration::from_secs(4)))
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_global(Some(Duration::from_secs(20)))
            .build()
            .into();
        Ok(Client {
            requester: None,
            token: None,
            dns: None,
            resolver: None,
            offline: (0, ""),
            agent,
            bases,
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
    /// worded for people. The system's DNS and routes serve every source: after a lookup or a
    /// connection with no route failed twice in a row, no request is sent for the rest of the
    /// run, as each would fail the same way.
    #[expect(clippy::result_large_err, reason = "one per paced request")]
    pub fn send(&mut self, s: &Source, req: Request<Vec<u8>>) -> Result<Reply, Live> {
        if let (2.., what) = self.offline {
            let message = format!(
                "Skipped: this computer's {what} failed twice in a row in this run. This is not a verdict on the number."
            );
            return Err(unavailable(s, message));
        }
        let host = req.uri().host().unwrap_or_default().to_string();
        let answer = self.fetch(s, req);
        self.offline = match answer.as_ref().err().and_then(offline) {
            Some(what) => (self.offline.0 + 1, what),
            None => (0, ""),
        };
        answer.map_err(|e| {
            let proxy = self.agent.config().proxy().map(|p| p.host().to_string());
            failure(s, &host, proxy.as_deref(), e)
        })
    }

    /// `send`, with a failure as the transport's own error.
    pub fn fetch(&mut self, s: &Source, mut req: Request<Vec<u8>>) -> Result<Reply, ureq::Error> {
        let gzip = ureq::http::HeaderValue::from_static("gzip");
        req.headers_mut().insert("accept-encoding", gzip);
        let at = {
            let mut last = LAST.lock().unwrap();
            if let Some(at) = last.get(s.name) {
                std::thread::sleep(s.gap.saturating_sub(at.elapsed()));
            }
            let now = Instant::now();
            last.insert(s.name, now);
            now
        };
        let since = (at - self.start).as_secs_f64();
        debug(format!(
            "{} {} {} (+{since:.3} s)",
            s.name,
            req.method(),
            req.uri()
        ));
        let answer = self.agent.run(req).and_then(|mut r| {
            let mut body = Vec::new();
            // ureq's limit errs when it is reached, so it allows one byte more than the cap.
            let wire = r.body_mut().with_config().limit(LIMIT + 1).reader();
            wire.take(LIMIT + 1).read_to_end(&mut body)?;
            // Coding names are case-insensitive (RFC 9110 §8.4.1); ureq would decode only
            // "gzip". An answer that doesn't decode reaches the source as sent, unreadable.
            let coding = r.headers().get("content-encoding");
            let coding = coding.and_then(|v| v.to_str().ok()).unwrap_or_default();
            if ["gzip", "x-gzip"].contains(&coding.trim().to_ascii_lowercase().as_str()) {
                let mut decoded = Vec::new();
                let gz = MultiGzDecoder::new(&body[..])
                    .take(LIMIT + 1)
                    .read_to_end(&mut decoded);
                if gz.is_ok() {
                    body = decoded;
                }
            }
            if body.len() as u64 > LIMIT {
                return Err(ureq::Error::BodyExceedsLimit(LIMIT));
            }
            let body = String::from_utf8_lossy(&body).into_owned();
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
                    retry_after: header("retry-after").and_then(retry_after),
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
    /// is tried again (after 2 s, then 4 s, or the 429's `Retry-After`), never a timeout. A
    /// request that can't be built, as when a registry's token holds a line break, isn't sent.
    pub fn ask(
        &mut self,
        s: &Source,
        req: impl Fn() -> ureq::http::Result<Request<Vec<u8>>>,
        read: impl Fn(&Reply) -> Result<Live, Error>,
    ) -> Result<Live, Error> {
        let mut tries = 0;
        loop {
            let Ok(req) = req() else {
                let message = format!(
                    "Couldn't build the request to {}. This is not a verdict on the number.",
                    s.name
                );
                return Ok(unavailable(s, message));
            };
            let live = match self.send(s, req) {
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

/// `Retry-After` in seconds from now: delta-seconds or an HTTP date (RFC 9110 §10.2.3).
fn retry_after(v: &str) -> Option<u64> {
    let v = v.trim();
    v.parse().ok().or_else(|| {
        let at = jiff::fmt::rfc2822::DateTimeParser::new()
            .parse_timestamp(v)
            .ok()?;
        let wait = jiff::Timestamp::now().duration_until(at).as_secs();
        Some(u64::try_from(wait).unwrap_or(0))
    })
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

/// `s`'s base URL: its override `url` when set, which must be an https:// URL, or http:// to
/// this computer.
fn base(s: &Source, url: Option<String>) -> Result<String, Error> {
    let Some(url) = url.filter(|v| !v.is_empty()) else {
        return Ok(s.base.into());
    };
    let url = url.trim_end_matches('/');
    let https = url.len() > "https://".len() && url.starts_with("https://");
    let uri = url.parse::<ureq::http::Uri>();
    let message = if !https && !local_http(url) {
        format!(
            "{} must start with https:// (http:// only for 127.0.0.1, [::1] or localhost).",
            s.var
        )
    } else if uri.is_err() {
        format!("{} isn't a valid URL.", s.var)
    } else if uri.is_ok_and(|u| u.authority().is_some_and(|a| a.as_str().contains('@'))) {
        // It would be sent to the registry and could be printed: never take one.
        format!("{} must not hold a user name or password.", s.var)
    } else {
        return Ok(url.into());
    };
    Err(usage(
        message,
        "Fix it, or unset it to use the official address.",
    ))
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
    // Redacted before the cut, so the cut can't leave part of a secret.
    let head: String = redact(body).chars().take(500).collect();
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
        E::Timeout(T::Resolve) => reach("DNS lookup took longer than 4 s", 0),
        E::Timeout(T::Connect) => reach("no answer within 5 s", 0),
        E::Timeout(_) => unavailable(s, format!("{} took longer than 20 s.", s.name)),
        E::HostNotFound => reach("host not found", 0),
        E::Io(e) if lookup_failed(&e) => reach("DNS lookup failed", 0),
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

/// What failed on this computer rather than at the registry: its DNS, or its network (no
/// route to any registry).
fn offline(e: &ureq::Error) -> Option<&'static str> {
    match e {
        ureq::Error::Timeout(ureq::Timeout::Resolve) => Some("DNS"),
        ureq::Error::Io(e) if lookup_failed(e) => Some("DNS"),
        ureq::Error::Io(e) if e.kind() == std::io::ErrorKind::NetworkUnreachable => Some("network"),
        _ => None,
    }
}

/// Whether the system couldn't look up a host name: std words it on Unix; Windows gives
/// WSAHOST_NOT_FOUND to WSANO_DATA.
fn lookup_failed(e: &std::io::Error) -> bool {
    e.raw_os_error()
        .is_some_and(|c| (11001..=11004).contains(&c))
        || e.to_string().starts_with("failed to lookup address")
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
        for bad in ["http://127.0.0.1:1/bad path", "https://exa mple.org"] {
            let e = with(bad).unwrap_err();
            assert_eq!(e.message, "VALIDATE_IDENTIFIER_TEST_URL isn't a valid URL.");
        }
        for bad in [
            "https://user:pa55@example.org/vies",
            "https://user@example.org",
        ] {
            let e = with(bad).unwrap_err();
            assert_eq!(
                e.message,
                "VALIDATE_IDENTIFIER_TEST_URL must not hold a user name or password."
            );
        }
        for ok in ["https://example.org/a@b", "https://example.org?a@b"] {
            assert!(with(ok).is_ok(), "{ok}");
        }
        assert_eq!(with("").unwrap(), "https://example.org");
    }

    #[test]
    fn sends_nothing_once_the_system_dns_failed_twice() {
        let mut c = Client::new(&[&TEST]).unwrap();
        c.offline = (2, "DNS");
        let req = Request::get("https://example.org/")
            .body(Vec::new())
            .unwrap();
        let skipped = c.send(&TEST, req).err().unwrap();
        assert_eq!(
            skipped.message,
            "Skipped: this computer's DNS failed twice in a row in this run. This is not a verdict on the number."
        );
        assert!(!LAST.lock().unwrap().contains_key(TEST.name));
    }

    #[test]
    fn a_network_without_a_route_counts_toward_the_run_wide_skip() {
        let io = |e: std::io::Error| offline(&ureq::Error::Io(e));
        assert_eq!(
            io(std::io::ErrorKind::NetworkUnreachable.into()),
            Some("network")
        );
        #[cfg(windows)]
        assert_eq!(
            io(std::io::Error::from_raw_os_error(10051)),
            Some("network")
        );
        #[cfg(target_os = "linux")]
        assert_eq!(io(std::io::Error::from_raw_os_error(101)), Some("network"));
        assert_eq!(io(std::io::Error::from_raw_os_error(11001)), Some("DNS"));
        assert_eq!(io(std::io::ErrorKind::ConnectionReset.into()), None);
        assert_eq!(offline(&ureq::Error::ConnectionFailed), None);
        let mut c = Client::new(&[&TEST]).unwrap();
        c.offline = (2, "network");
        let req = Request::get("https://example.org/")
            .body(Vec::new())
            .unwrap();
        assert_eq!(
            c.send(&TEST, req).err().unwrap().message,
            "Skipped: this computer's network failed twice in a row in this run. This is not a verdict on the number."
        );
    }

    #[test]
    fn pacing_holds_across_clients() {
        use std::io::{Read, Write};
        const PACED: Source = Source {
            name: "Paced",
            gap: Duration::from_millis(300),
            ..TEST
        };
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", server.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut s in server.incoming().flatten() {
                let _ = s.read(&mut [0; 1024]);
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            }
        });
        let get = || Request::get(&url).body(Vec::new()).unwrap();
        let start = Instant::now();
        Client::new(&[]).unwrap().fetch(&PACED, get()).unwrap();
        Client::new(&[]).unwrap().fetch(&PACED, get()).unwrap();
        assert!(start.elapsed() >= PACED.gap);
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
        for lookup in [
            std::io::Error::other("failed to lookup address information: Try again"),
            std::io::Error::from_raw_os_error(11002),
        ] {
            let no_dns = f(ureq::Error::Io(lookup));
            assert_eq!(
                (no_dns.message.as_str(), no_dns.retries),
                (
                    "Couldn't reach ec.europa.eu: DNS lookup failed. This is not a verdict on the number.",
                    0
                )
            );
        }
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
