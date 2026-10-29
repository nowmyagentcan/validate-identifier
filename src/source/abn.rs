//! ABN Lookup's JSON service, with the user's own GUID. The answer is JSONP; the GUID never
//! appears in any output.

use std::time::Duration;

use serde_json::{Value, json};
use ureq::http::Request;

use crate::item::{Error, Item, Live, LiveStatus, minute};
use crate::live::{self, Client, Reply, Source, encode, unreadable};

pub const ABN: Source = Source {
    name: "ABN Lookup",
    var: "VALIDATE_IDENTIFIER_ABN_URL",
    base: "https://abr.business.gov.au",
    gap: Duration::from_millis(500),
};

const HINT: &str = "Set ABN_LOOKUP_GUID (free at abr.business.gov.au/Tools/WebServices).";

fn not_configured(message: &str) -> Live {
    Live {
        hint: Some(HINT),
        ..Live::new(ABN.name, LiveStatus::NotConfigured, message)
    }
}

pub fn ask(c: &mut Client, items: &[&Item]) -> Result<Vec<Live>, Error> {
    let guid = std::env::var("ABN_LOOKUP_GUID").unwrap_or_default();
    if guid.is_empty() {
        return Ok(vec![not_configured(
            "ABN checks need your own ABN Lookup GUID.",
        )]);
    }
    let guid = encode(&guid);
    live::secret(&guid);
    let abn = items[0].value.as_deref().unwrap_or_default();
    let url = c.url(
        &ABN,
        &format!("/json/AbnDetails.aspx?abn={abn}&guid={guid}"),
    );
    let req = || Request::get(&url).body(Vec::new());
    Ok(vec![c.ask(&ABN, req, |r| Ok(answer(r)))?])
}

/// One answer as a live status (spec §3.6).
fn answer(r: &Reply) -> Live {
    if r.status != 200 {
        return r.failed(&ABN);
    }
    // JSONP: `callback({...})`.
    let json = match (r.body.find('('), r.body.rfind(')')) {
        (Some(start), Some(end)) if start < end => &r.body[start + 1..end],
        _ => r.body.as_str(),
    };
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return unreadable(&ABN, &r.body);
    };
    let field = |k: &str| v[k].as_str().filter(|s| !s.is_empty());
    if field("Message").is_some_and(|m| m.to_ascii_uppercase().contains("GUID")) {
        return not_configured("ABN Lookup didn't recognise ABN_LOOKUP_GUID.");
    }
    let now = Live::new(ABN.name, LiveStatus::Active, "");
    let (status, message) = match (v["Abn"].as_str(), field("AbnStatus")) {
        (Some(""), _) => (LiveStatus::NotFound, "Not found in ABN Lookup.".to_string()),
        (Some(_), Some("Active")) => (
            LiveStatus::Active,
            format!("Active in ABN Lookup ({}).", minute(&now.checked_at)),
        ),
        (Some(_), Some(other)) => (
            LiveStatus::Inactive,
            format!("ABN Lookup lists this ABN as {other}."),
        ),
        _ => return unreadable(&ABN, &r.body),
    };
    Live {
        status,
        message,
        data: json!({
            "entity_name": field("EntityName"),
            "entity_type": field("EntityTypeName"),
            "abn_status": field("AbnStatus"),
            "abn_status_from": field("AbnStatusEffectiveFrom"),
            "gst_from": field("Gst"),
            "state": field("AddressState"),
            "postcode": field("AddressPostcode"),
        }),
        ..now
    }
}
