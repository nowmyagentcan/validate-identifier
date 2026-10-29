//! GLEIF's LEI records API, up to 100 LEIs per request.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Value, json};
use ureq::http::Request;

use crate::item::{Error, Item, Live, LiveStatus, minute};
use crate::live::{Client, Source, unreadable};

pub const GLEIF: Source = Source {
    name: "GLEIF",
    var: "VALIDATE_IDENTIFIER_GLEIF_URL",
    base: "https://api.gleif.org",
    gap: Duration::from_millis(1100),
};

/// Registration states of an LEI that is in use.
const LIVE: [&str; 3] = ["ISSUED", "PENDING_TRANSFER", "PENDING_ARCHIVAL"];

pub fn ask(c: &mut Client, items: &[&Item]) -> Result<Vec<Live>, Error> {
    let leis: Vec<&str> = items.iter().filter_map(|i| i.value.as_deref()).collect();
    let path = format!(
        "/api/v1/lei-records?filter%5Blei%5D={}&page%5Bsize%5D=100",
        leis.join(",")
    );
    let url = c.url(&GLEIF, &path);
    let req = || {
        Request::get(&url)
            .header("Accept", "application/vnd.api+json")
            .body(Vec::new())
    };
    let records = RefCell::new(None);
    let live = c.ask(&GLEIF, req, |r| {
        let v: Value = serde_json::from_str(&r.body).unwrap_or_default();
        Ok(match (r.status, v["data"].as_array()) {
            (200, Some(data)) => {
                let by_lei = data.iter().map(|d| {
                    let a = &d["attributes"];
                    let lei = a["lei"].as_str().filter(|l| leis.contains(l))?;
                    Some((lei.to_string(), a.clone()))
                });
                // A record without one of the LEIs asked for could be any of them: the answer
                // is unreadable.
                let Some(by_lei) = by_lei.collect::<Option<HashMap<_, _>>>() else {
                    return Ok(unreadable(&GLEIF, &r.body));
                };
                records.replace(Some(by_lei));
                Live::new(GLEIF.name, LiveStatus::Active, "")
            }
            (200, None) => unreadable(&GLEIF, &r.body),
            (404, _) => {
                records.replace(Some(HashMap::new()));
                Live::new(GLEIF.name, LiveStatus::NotFound, "")
            }
            _ => r.failed(&GLEIF),
        })
    })?;
    let Some(records) = records.into_inner() else {
        return Ok(vec![live; leis.len()]);
    };
    Ok(leis.iter().map(|lei| record(records.get(*lei))).collect())
}

/// One LEI's status from its record, or `not_found` when the answer left it out.
fn record(a: Option<&Value>) -> Live {
    let Some(a) = a else {
        return Live::new(GLEIF.name, LiveStatus::NotFound, "Not found in GLEIF.");
    };
    let (e, r) = (&a["entity"], &a["registration"]);
    let (Some(registration), Some(entity)) = (r["status"].as_str(), e["status"].as_str()) else {
        return unreadable(&GLEIF, &a.to_string());
    };
    let now = Live::new(GLEIF.name, LiveStatus::Active, "");
    let (status, message) = if LIVE.contains(&registration) && entity == "ACTIVE" {
        let when = minute(&now.checked_at);
        (
            LiveStatus::Active,
            format!("Registration {registration} in GLEIF ({when})."),
        )
    } else {
        (
            LiveStatus::Inactive,
            format!("GLEIF lists this LEI as {registration} (entity {entity})."),
        )
    };
    Live {
        status,
        message,
        data: json!({
            "legal_name": e["legalName"]["name"],
            "entity_status": e["status"],
            "registration_status": r["status"],
            "next_renewal_date": r["nextRenewalDate"],
            "managing_lou": r["managingLou"],
        }),
        ..now
    }
}
