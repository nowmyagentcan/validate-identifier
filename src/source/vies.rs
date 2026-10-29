//! VIES, the EU's VAT number check, for EU and XI numbers. Its errors arrive as
//! `errorWrappers`, often with HTTP 200.

use std::time::Duration;

use serde_json::{Value, json};
use ureq::http::Request;

use crate::check::usage;
use crate::item::{Error, Item, Live, LiveStatus, at, minute};
use crate::live::{Client, Reply, Source, unreadable};

pub const VIES: Source = Source {
    name: "VIES",
    var: "VALIDATE_IDENTIFIER_VIES_URL",
    base: "https://ec.europa.eu",
    gap: Duration::from_secs(1),
};

const PATH: &str = "/taxation_customs/vies/rest-api/check-vat-number";

/// The codes that mean "too many requests right now".
const BUSY: [&str; 5] = [
    "SERVER_BUSY",
    "GLOBAL_MAX_CONCURRENT_REQ",
    "GLOBAL_MAX_CONCURRENT_REQ_TIME",
    "MS_MAX_CONCURRENT_REQ",
    "MS_MAX_CONCURRENT_REQ_TIME",
];

pub fn ask(c: &mut Client, items: &[&Item]) -> Result<Vec<Live>, Error> {
    let d = &items[0].details;
    let prefix = d["prefix"].as_str().unwrap_or_default();
    let mut body = json!({"countryCode": prefix, "vatNumber": d["number"]});
    if let Some((p, n)) = c.requester.as_ref().filter(|(p, _)| uses(p)) {
        body["requesterMemberStateCode"] = p.as_str().into();
        body["requesterNumber"] = n.as_str().into();
    }
    let body = body.to_string();
    let url = c.url(&VIES, PATH);
    let req = || {
        Request::post(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(body.clone().into_bytes())
            .expect("a valid request")
    };
    Ok(vec![c.ask(&VIES, req, |r| answer(r, prefix))?])
}

/// Whether VIES takes a requester with this prefix: EU and XI numbers.
pub fn uses(requester_prefix: &str) -> bool {
    !matches!(requester_prefix, "GB" | "CHE")
}

/// One answer as a live status (spec §3.6).
fn answer(r: &Reply, prefix: &str) -> Result<Live, Error> {
    let v: Value = serde_json::from_str(&r.body).unwrap_or_default();
    if let Some(code) = v["errorWrappers"][0]["error"].as_str() {
        let (status, message) = match code {
            "INVALID_REQUESTER_INFO" => {
                let hint = "Pass your own valid VAT number, or leave out --requester.";
                return Err(usage("VIES rejected your --requester number.", hint));
            }
            "INVALID_INPUT" => (
                LiveStatus::NotFound,
                format!("VIES says this isn't a valid {prefix} VAT number (INVALID_INPUT)."),
            ),
            c if BUSY.contains(&c) => (
                LiveStatus::RateLimited,
                format!("VIES is busy ({c}). Try again in a minute."),
            ),
            c => (
                LiveStatus::Unavailable,
                format!(
                    "VIES didn't answer for {prefix} ({c}). This is not a verdict on the number. Try again later."
                ),
            ),
        };
        let busy = status == LiveStatus::RateLimited;
        return Ok(Live {
            code: Some(code.into()),
            retry_after: busy.then_some(60),
            retries: if busy { 2 } else { 0 },
            ..Live::new(VIES.name, status, message)
        });
    }
    if r.status != 200 {
        return Ok(r.failed(&VIES));
    }
    let Some(valid) = v["valid"].as_bool() else {
        return Ok(unreadable(&VIES, &r.body));
    };
    let checked_at = at(v["requestDate"].as_str());
    let field = |k: &str| v[k].as_str().filter(|s| !s.is_empty() && *s != "---");
    let (status, message) = match valid {
        true => (
            LiveStatus::Active,
            format!("Registered for VAT (VIES, {}).", minute(&checked_at)),
        ),
        false => (
            LiveStatus::NotFound,
            "VIES has no VAT registration for this number.".into(),
        ),
    };
    Ok(Live {
        checked_at,
        data: json!({
            "name": field("name"),
            "address": field("address"),
            "consultation_number": field("requestIdentifier"),
        }),
        ..Live::new(VIES.name, status, message)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_codes_are_rate_limited_and_tried_twice_more() {
        for code in BUSY {
            let r = Reply {
                status: 200,
                body: json!({"actionSucceed": false, "errorWrappers": [{"error": code}]})
                    .to_string(),
                retry_after: None,
            };
            let live = answer(&r, "DE").unwrap();
            assert_eq!(live.status, LiveStatus::RateLimited, "{code}");
            assert_eq!((live.retry_after, live.retries), (Some(60), 2), "{code}");
            assert_eq!(
                live.message,
                format!("VIES is busy ({code}). Try again in a minute.")
            );
        }
    }

    #[test]
    fn unknown_error_codes_are_never_a_verdict() {
        let r = Reply {
            status: 200,
            body: r#"{"actionSucceed":false,"errorWrappers":[{"error":"NEW_CODE"}]}"#.into(),
            retry_after: None,
        };
        let live = answer(&r, "DE").unwrap();
        assert_eq!(live.status, LiveStatus::Unavailable);
        assert_eq!(live.code.as_deref(), Some("NEW_CODE"));
        let empty = Reply {
            body: "{}".into(),
            ..r
        };
        assert_eq!(
            answer(&empty, "DE").unwrap().message,
            "VIES sent a response we couldn't read."
        );
    }
}
