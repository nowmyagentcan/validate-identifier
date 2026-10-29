//! Peppol registration: each candidate participant ID's NAPTR record in the SML (DNS) names
//! the participant's SMP.

use std::time::Duration;

use hickory_resolver::proto::rr::{RData, RecordType};
use serde_json::{Value, json};

use crate::item::{Error, Item, Live, LiveStatus, Warning, minute};
use crate::live::{Client, Source, local_http, unavailable};
use crate::peppol::{Naptr, sml_name, smp_url};
use crate::source::dns::{Records, failed};

pub const SML: Source = Source {
    name: "Peppol SML",
    var: "",
    base: "",
    gap: Duration::ZERO,
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
    for id in &candidates {
        let registered = match find(c, id, zone.as_deref()) {
            Sml::Registered(smp) => Some(smp),
            Sml::NotRegistered => None,
            Sml::Failed(live) => {
                failure.get_or_insert(live);
                continue;
            }
        };
        participants.push(json!({
            "id": id,
            "registered": registered.is_some(),
            "smp": registered,
            "document_types": null,
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
        Live { message, ..now }
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
