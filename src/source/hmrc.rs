//! HMRC's "Check a UK VAT number" v2, with the user's own application credentials (the
//! bearer token lives in memory for the run), and its open EORI check, 10 numbers a request.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Value, json};
use ureq::http::Request;

use crate::check::usage;
use crate::item::{Error, Item, Live, LiveStatus, at, minute};
use crate::live::{self, Client, Reply, Source, encode, unavailable, unreadable};

pub const HMRC: Source = Source {
    name: "HMRC",
    var: "VALIDATE_IDENTIFIER_HMRC_URL",
    base: "https://api.service.hmrc.gov.uk",
    gap: Duration::from_millis(400),
};

pub fn ask(c: &mut Client, items: &[&Item]) -> Result<Vec<Live>, Error> {
    let token = match token(c)? {
        Ok(token) => token,
        Err(no) => return Ok(vec![no]),
    };
    let vrn = items[0].details["number"].as_str().unwrap_or_default();
    let requester = c.requester.as_ref().filter(|(p, _)| p == "GB");
    let mut path = format!("/organisations/vat/check-vat-number/lookup/{vrn}");
    if let Some((_, number)) = requester {
        path = format!("{path}/{number}");
    }
    let url = c.url(&HMRC, &path);
    let req = || {
        Request::get(&url)
            .header("Accept", "application/vnd.hmrc.2.0+json")
            .header("Authorization", format!("Bearer {token}"))
            .body(Vec::new())
            .expect("a valid request")
    };
    let with_requester = requester.is_some();
    Ok(vec![c.ask(&HMRC, req, |r| answer(r, with_requester))?])
}

/// The bearer token, fetched once per run; without credentials, or with rejected ones, the
/// `not_configured` answer instead.
fn token(c: &mut Client) -> Result<Result<String, Live>, Error> {
    if let Some(token) = &c.token {
        return Ok(Ok(token.clone()));
    }
    let var = |name| std::env::var(name).ok().filter(|v| !v.is_empty());
    let (Some(id), Some(secret)) = (var("HMRC_CLIENT_ID"), var("HMRC_CLIENT_SECRET")) else {
        return Ok(Err(Live {
            hint: Some("Set HMRC_CLIENT_ID and HMRC_CLIENT_SECRET (see the README)."),
            ..Live::new(
                HMRC.name,
                LiveStatus::NotConfigured,
                "UK VAT checks need your own HMRC credentials.",
            )
        }));
    };
    live::secret(&id);
    live::secret(&secret);
    let form = format!(
        "client_id={}&client_secret={}&grant_type=client_credentials&scope=read%3Avat",
        encode(&id),
        encode(&secret)
    );
    let url = c.url(&HMRC, "/oauth/token");
    let req = || {
        Request::post(&url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .body(form.clone().into_bytes())
            .expect("a valid request")
    };
    let got = RefCell::new(None);
    let live = c.ask(&HMRC, req, |r| {
        let v: Value = serde_json::from_str(&r.body).unwrap_or_default();
        Ok(match (r.status, v["access_token"].as_str()) {
            (200, Some(token)) => {
                live::secret(token);
                got.replace(Some(token.to_string()));
                Live::new(HMRC.name, LiveStatus::Active, "")
            }
            // Never shown under --debug: a token may be in it.
            (200, None) => unavailable(&HMRC, "HMRC sent a response we couldn't read."),
            (400 | 401, _) => rejected(v["error"].as_str()),
            _ => r.failed(&HMRC),
        })
    })?;
    c.token = got.into_inner();
    Ok(c.token.clone().ok_or(live))
}

fn rejected(code: Option<&str>) -> Live {
    Live {
        code: code.map(String::from),
        hint: Some(
            "Check HMRC_CLIENT_ID, HMRC_CLIENT_SECRET and VALIDATE_IDENTIFIER_HMRC_URL (sandbox credentials need the test URL).",
        ),
        ..Live::new(
            HMRC.name,
            LiveStatus::NotConfigured,
            "HMRC rejected the client ID or secret.",
        )
    }
}

/// One lookup answer as a live status (spec §3.6).
fn answer(r: &Reply, with_requester: bool) -> Result<Live, Error> {
    let v: Value = serde_json::from_str(&r.body).unwrap_or_default();
    let code = v["code"].as_str().map(String::from);
    let (status, message) = match r.status {
        200 => {
            let target = &v["target"];
            let Some(name) = target["name"].as_str() else {
                return Ok(unreadable(&HMRC, &r.body));
            };
            let a = &target["address"];
            let address: Vec<&str> = ["line1", "line2", "line3", "line4", "line5", "postcode"]
                .iter()
                .filter_map(|k| a[k].as_str())
                .collect();
            let checked_at = at(v["processingDate"].as_str());
            let message = format!("Registered for VAT (HMRC, {}).", minute(&checked_at));
            return Ok(Live {
                checked_at,
                data: json!({
                    "name": name,
                    "address": address.join(", "),
                    "consultation_number": v["consultationNumber"],
                }),
                ..Live::new(HMRC.name, LiveStatus::Active, message)
            });
        }
        404 => (
            LiveStatus::NotFound,
            "HMRC has no VAT registration for this number.".to_string(),
        ),
        400 => (
            LiveStatus::Unavailable,
            format!(
                "HMRC didn't accept the request ({}). This is not a verdict on the number.",
                code.as_deref().unwrap_or("400")
            ),
        ),
        401 => return Ok(rejected(code.as_deref())),
        403 if with_requester => {
            let hint = "Pass your own valid VAT number, or leave out --requester.";
            return Err(usage("HMRC rejected your --requester number.", hint));
        }
        _ => return Ok(r.failed(&HMRC)),
    };
    Ok(Live {
        code,
        ..Live::new(HMRC.name, status, message)
    })
}

/// GB EORI numbers, up to 10 per request; no credentials needed.
pub fn eori(c: &mut Client, items: &[&Item]) -> Result<Vec<Live>, Error> {
    let eoris: Vec<&str> = items.iter().filter_map(|i| i.value.as_deref()).collect();
    let body = json!({ "eoris": eoris }).to_string();
    let url = c.url(&HMRC, "/customs/eori/lookup/check-multiple-eori");
    let req = || {
        Request::post(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/vnd.hmrc.1.0+json")
            .body(body.clone().into_bytes())
            .expect("a valid request")
    };
    let answers = RefCell::new(None);
    let live = c.ask(&HMRC, req, |r| {
        let v: Value = serde_json::from_str(&r.body).unwrap_or_default();
        Ok(match (r.status, v.as_array()) {
            (200, Some(list)) => {
                let by_eori = list.iter().filter_map(|a| Some((a["eori"].as_str()?, a)));
                let by_eori: HashMap<_, _> = by_eori.map(|(e, a)| (e.to_string(), eori_live(a))).collect();
                answers.replace(Some(by_eori));
                Live::new(HMRC.name, LiveStatus::Active, "")
            }
            (200, None) => unreadable(&HMRC, &r.body),
            (400, _) => {
                let why = v["message"].as_str().unwrap_or("HTTP 400");
                Live {
                    code: v["code"].as_str().map(String::from),
                    ..unavailable(
                        &HMRC,
                        format!(
                            "HMRC didn't accept the request ({why}). This is not a verdict on the number."
                        ),
                    )
                }
            }
            _ => r.failed(&HMRC),
        })
    })?;
    let Some(answers) = answers.into_inner() else {
        return Ok(vec![live; eoris.len()]);
    };
    Ok(eoris
        .iter()
        .map(|e| match answers.get(*e) {
            Some(live) => live.clone(),
            None => unreadable(&HMRC, "the answer left out an EORI number"),
        })
        .collect())
}

/// One EORI's answer as a live status, with the trader's name and address when they agreed
/// to publish them.
fn eori_live(a: &Value) -> Live {
    let checked_at = at(a["processingDate"].as_str());
    let d = &a["companyDetails"];
    let address: Vec<&str> = ["streetAndNumber", "cityName", "postcode"]
        .iter()
        .filter_map(|k| d["address"][k].as_str())
        .collect();
    let (status, message) = match a["valid"].as_bool() {
        Some(true) => (
            LiveStatus::Active,
            format!("Valid EORI number (HMRC, {}).", minute(&checked_at)),
        ),
        Some(false) => (
            LiveStatus::NotFound,
            "HMRC has no EORI registration for this number.".to_string(),
        ),
        None => return unreadable(&HMRC, &a.to_string()),
    };
    Live {
        checked_at,
        data: json!({
            "name": d["traderName"],
            "address": (!address.is_empty()).then(|| address.join(", ")),
        }),
        ..Live::new(HMRC.name, status, message)
    }
}
