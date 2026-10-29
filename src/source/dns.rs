//! DNS through the system's resolver, or the server `--dns` names: mail servers for email
//! domains here, NAPTR records for Peppol. Lookups run one at a time on a single-thread
//! runtime; each try waits 5 s, and a failed try is repeated once.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use hickory_resolver::config::{NameServerConfig, ResolverConfig};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::net::{DnsError, NetError};
use hickory_resolver::proto::rr::{RData, RecordType};
use hickory_resolver::{ResolverBuilder, TokioResolver};
use serde_json::json;
use tokio::runtime::Runtime;

use crate::check::usage;
use crate::item::{Error, Item, Live, LiveStatus, minute};
use crate::live::{Client, Source, unavailable};

pub const DNS: Source = Source {
    name: "DNS",
    var: "",
    base: "",
    gap: Duration::ZERO,
};

/// What a name has: records of the type asked for, nothing of that type, or no name at all.
pub enum Records {
    Some(Vec<RData>),
    None,
    NxDomain,
}

pub struct Dns {
    runtime: Runtime,
    resolver: TokioResolver,
}

impl Dns {
    /// A resolver asking `server`, or the system's DNS servers.
    pub fn new(server: Option<SocketAddr>) -> Result<Dns, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let mut builder: ResolverBuilder<_> = match server {
            Some(at) => {
                let mut ns = NameServerConfig::udp_and_tcp(at.ip());
                for c in &mut ns.connections {
                    c.port = at.port();
                }
                let config = ResolverConfig::from_name_servers(vec![ns]);
                TokioResolver::builder_with_config(config, TokioRuntimeProvider::default())
            }
            None => TokioResolver::builder_tokio()
                .map_err(|e| format!("no DNS settings found ({e})"))?,
        };
        let o = builder.options_mut();
        o.timeout = Duration::from_secs(5);
        o.attempts = 1;
        let _inside = runtime.enter();
        let resolver = builder.build().map_err(|e| e.to_string())?;
        Ok(Dns { runtime, resolver })
    }

    /// `name`'s records of type `t`; a failure is its reason (`SERVFAIL`, `timeout`, …).
    pub fn lookup(&self, name: &str, t: RecordType) -> Result<Records, String> {
        let fqdn = format!("{}.", name.trim_end_matches('.'));
        let answer = self
            .runtime
            .block_on(self.resolver.lookup(fqdn.as_str(), t));
        match answer {
            Ok(found) => {
                let data: Vec<RData> = found
                    .answers()
                    .iter()
                    .filter(|r| r.record_type() == t)
                    .map(|r| r.data.clone())
                    .collect();
                Ok(match data.is_empty() {
                    true => Records::None,
                    false => Records::Some(data),
                })
            }
            Err(e) if e.is_nx_domain() => Ok(Records::NxDomain),
            Err(e) if e.is_no_records_found() => Ok(Records::None),
            Err(NetError::Dns(DnsError::ResponseCode(code))) => {
                Err(format!("{code:?}").to_ascii_uppercase())
            }
            Err(NetError::Timeout) => Err("timeout".into()),
            Err(NetError::Io(e)) => Err(e.kind().to_string()),
            Err(NetError::NoConnections) => Err("no DNS server answered".into()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// `--dns IP[:PORT]` as an address; port 53 when left out.
pub fn server(raw: &str) -> Result<SocketAddr, Error> {
    raw.parse()
        .or_else(|_| raw.parse::<IpAddr>().map(|ip| SocketAddr::new(ip, 53)))
        .map_err(|_| {
            usage(
                format!("--dns {raw} isn't an IP address."),
                "Pass an IP address, with a port if it isn't 53, e.g. 1.1.1.1 or 127.0.0.1:5353.",
            )
        })
}

/// A lookup that got no answer, with the resolver's code when it gave one.
pub fn failed(s: &Source, message: String, why: &str) -> Live {
    let code = why.bytes().all(|b| b.is_ascii_uppercase());
    Live {
        code: code.then(|| why.to_string()),
        ..unavailable(s, message)
    }
}

/// Does an email domain take mail: its MX records, else its own address (RFC 5321 §5.1),
/// unless a null MX says it takes none (RFC 7505).
pub fn ask(c: &mut Client, items: &[&Item]) -> Result<Vec<Live>, Error> {
    let domain = items[0].details["ascii_domain"]
        .as_str()
        .unwrap_or_default();
    Ok(vec![mail(c, domain)])
}

fn mail(c: &mut Client, domain: &str) -> Live {
    let fail = |why: String| {
        let message = format!("Couldn't look up mail servers for {domain}: {why}.");
        failed(&DNS, message, &why)
    };
    let now = Live::new(DNS.name, LiveStatus::Active, "");
    let when = minute(&now.checked_at);
    let answer = |status, kind, mx: Vec<(u16, String)>, message: String| Live {
        status,
        message,
        data: json!({
            "kind": kind,
            "mx": mx.iter().map(|(p, h)| json!({"preference": p, "host": h})).collect::<Vec<_>>(),
        }),
        ..now.clone()
    };
    let none = |code: Option<&str>, message| Live {
        code: code.map(String::from),
        ..answer(LiveStatus::NotFound, "none", Vec::new(), message)
    };
    let mut mx: Vec<(u16, String)> = match c.lookup(domain, RecordType::MX) {
        Err(why) => return fail(why),
        Ok(Records::NxDomain) => {
            return none(
                Some("NXDOMAIN"),
                format!("{domain} doesn't exist (NXDOMAIN)."),
            );
        }
        Ok(Records::Some(records)) => records
            .iter()
            .filter_map(|r| match r {
                RData::MX(m) => Some((m.preference, m.exchange.to_ascii())),
                _ => None,
            })
            .collect(),
        Ok(Records::None) => Vec::new(),
    };
    mx.sort();
    if let [(_, host)] = mx.as_slice()
        && host == "."
    {
        let message = "This domain says it accepts no email.".into();
        return answer(LiveStatus::Inactive, "null_mx", mx, message);
    }
    if !mx.is_empty() {
        for (_, host) in &mut mx {
            host.truncate(host.trim_end_matches('.').len());
        }
        let message = format!("Mail servers found for {domain} (DNS, {when}).");
        return answer(LiveStatus::Active, "mx", mx, message);
    }
    for t in [RecordType::A, RecordType::AAAA] {
        match c.lookup(domain, t) {
            Err(why) => return fail(why),
            Ok(Records::Some(_)) => {
                let message = format!(
                    "{domain} lists no mail servers, so mail goes to its own address (DNS, {when})."
                );
                return answer(LiveStatus::Active, "implicit", mx, message);
            }
            Ok(_) => {}
        }
    }
    none(
        None,
        format!("{domain} has no mail servers and no address."),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_servers_are_ip_addresses_with_an_optional_port() {
        assert_eq!(server("1.1.1.1").unwrap().to_string(), "1.1.1.1:53");
        assert_eq!(
            server("127.0.0.1:5353").unwrap().to_string(),
            "127.0.0.1:5353"
        );
        assert_eq!(server("::1").unwrap().to_string(), "[::1]:53");
        assert_eq!(server("[::1]:5353").unwrap().to_string(), "[::1]:5353");
        let e = server("dns.example").unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            ("usage", "--dns dns.example isn't an IP address.")
        );
    }
}
