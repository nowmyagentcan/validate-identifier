//! Peppol registration: each candidate participant ID's NAPTR record in the SML (DNS) names
//! the participant's SMP, whose ServiceGroup lists the document types it accepts.

use std::time::Duration;

use hickory_resolver::proto::rr::{RData, RecordType};
use serde_json::{Value, json};
use ureq::http::Request;

use crate::item::{Error, Item, Live, LiveStatus, Warning, minute};
use crate::live::{Client, Source, encode, local_http, unavailable};
use crate::peppol::{Naptr, document_types, sml_name, smp_url};
use crate::source::dns::{Records, failed};

pub const SML: Source = Source {
    name: "Peppol SML",
    var: "",
    base: "",
    gap: Duration::ZERO,
};

pub const SMP: Source = Source {
    name: "Peppol SMP",
    var: "",
    base: "",
    gap: Duration::from_millis(500),
};

const ZONE: &str = "participant.sml.prod.tech.peppol.org";
const HINT: &str = "Try again, or pass --dns 1.1.1.1 if your network's DNS blocks NAPTR queries.";

/// What the SML says about one participant ID.
enum Sml {
    Registered(String),
    NotRegistered,
    Failed(Live),
}

pub fn ask(c: &mut Client, items: &[&Item]) -> Result<Vec<Live>, Error> {
    let item = items[0];
    let zone = std::env::var("VALIDATE_IDENTIFIER_PEPPOL_SML")
        .ok()
        .filter(|z| !z.is_empty());
    let candidates: Vec<&str> = item.details["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut participants = Vec::new();
    let mut failure = None;
    let mut warnings = Vec::new();
    for id in &candidates {
        let registered = match find(c, id, zone.as_deref()) {
            Sml::Registered(smp) => Some(smp),
            Sml::NotRegistered => None,
            Sml::Failed(live) => {
                failure.get_or_insert(live);
                continue;
            }
        };
        let types = match &registered {
            Some(smp) => service_group(c, smp, id, zone.is_some()).unwrap_or_else(|w| {
                if !warnings.contains(&w) {
                    warnings.push(w);
                }
                Value::Null
            }),
            None => Value::Null,
        };
        participants.push(json!({
            "id": id,
            "registered": registered.is_some(),
            "smp": registered,
            "document_types": types,
        }));
    }
    let found: Vec<&str> = participants
        .iter()
        .filter(|p| p["registered"] == true)
        .filter_map(|p| p["id"].as_str())
        .collect();
    let now = Live::new(SML.name, LiveStatus::Active, "");
    let when = minute(&now.checked_at);
    let live = if !found.is_empty() {
        let message = format!(
            "Registered in Peppol as {} (Peppol SML, {when}).",
            found.join(" and ")
        );
        Live {
            message,
            warnings,
            ..now
        }
    } else if let Some(failure) = failure {
        failure
    } else {
        let ids = candidates.join(" or ");
        // A VAT number, ABN, UID or LEI may be registered under a scheme we didn't try.
        let mapped = item.value.as_deref().is_some_and(|v| !v.contains(':'));
        let warnings = match mapped {
            true => vec![Warning {
                code: "peppol_candidates",
                message: format!(
                    "Not found under {ids}; it may be registered under another scheme (e.g. GLN 0088)."
                ),
            }],
            false => Vec::new(),
        };
        Live {
            status: LiveStatus::NotFound,
            message: format!("Not registered in Peppol under {ids}."),
            code: Some("NXDOMAIN".into()),
            warnings,
            ..now
        }
    };
    Ok(vec![Live {
        data: json!({ "participants": participants }),
        ..live
    }])
}

/// One participant ID's SML record: its SMP's base URL (https only; http to this computer
/// only while the SML zone is overridden for tests), or no record.
fn find(c: &mut Client, id: &str, zone: Option<&str>) -> Sml {
    let records = match c.lookup(&sml_name(id, zone.unwrap_or(ZONE)), RecordType::NAPTR) {
        Ok(Records::Some(records)) => records,
        Ok(Records::NxDomain | Records::None) => return Sml::NotRegistered,
        Err(why) => {
            let message = format!(
                "Couldn't look up the Peppol SML: {why}. This is not a verdict on registration."
            );
            return Sml::Failed(Live {
                hint: Some(HINT),
                ..failed(&SML, message, &why)
            });
        }
    };
    let naptr: Vec<Naptr> = records
        .iter()
        .filter_map(|r| match r {
            RData::NAPTR(n) => Some((n.order, n.preference, &*n.flags, &*n.services, &*n.regexp)),
            _ => None,
        })
        .collect();
    let url = smp_url(&naptr).filter(|url| {
        let https = url.starts_with("https://") || (zone.is_some() && local_http(url));
        let authority = url.split('/').nth(2).unwrap_or_default();
        https && !authority.is_empty() && !authority.contains('@') && !url.contains(['?', '#'])
    });
    match url {
        Some(url) => Sml::Registered(url),
        None => Sml::Failed(unavailable(
            &SML,
            format!("The SML record for {id} is malformed."),
        )),
    }
}

/// The document types the SMP at `smp` lists for `id`, following up to 3 redirects
/// (https only, or http to this computer in `test`); a warning when they can't be read.
fn service_group(c: &mut Client, smp: &str, id: &str, test: bool) -> Result<Value, Warning> {
    let smp_unavailable = |why: &str| Warning {
        code: "smp_unavailable",
        message: format!(
            "Registered in the Peppol SML, but its SMP didn't answer ({why}); document types unknown."
        ),
    };
    let participant = encode(&format!("iso6523-actorid-upis::{id}"));
    let mut url = format!("{}/{participant}", smp.trim_end_matches('/'));
    for _ in 0..=3 {
        let req = Request::get(&url)
            .header("Accept", "application/xml")
            .body(Vec::new())
            .map_err(|_| smp_unavailable("bad address"))?;
        let reply = c.fetch(&SMP, req).map_err(|e| match e {
            ureq::Error::Timeout(_) => smp_unavailable("timeout"),
            ureq::Error::Io(e) => smp_unavailable(&e.kind().to_string()),
            e => smp_unavailable(&e.to_string()),
        })?;
        match reply.status {
            200 => {
                let types = document_types(&reply.body)
                    .ok_or_else(|| smp_unavailable("an answer that isn't a ServiceGroup"))?;
                let types = types
                    .into_iter()
                    .map(|(id, label)| json!({"id": id, "label": label}));
                return Ok(types.collect());
            }
            404 => {
                return Err(Warning {
                    code: "smp_not_found",
                    message: "Registered in the Peppol SML, but its SMP has no record; document types unknown.".into(),
                });
            }
            300..=399 => {
                // A path alone stays on the same server.
                let origin: Vec<&str> = url.splitn(4, '/').take(3).collect();
                let next = reply
                    .location
                    .map(|l| match l.starts_with('/') {
                        true => format!("{}{l}", origin.join("/")),
                        false => l,
                    })
                    .filter(|l| l.starts_with("https://") || (test && local_http(l)));
                url = next.ok_or_else(|| smp_unavailable("a redirect to a non-https address"))?;
            }
            status => return Err(smp_unavailable(&format!("HTTP {status}"))),
        }
    }
    Err(smp_unavailable("more than 3 redirects"))
}
