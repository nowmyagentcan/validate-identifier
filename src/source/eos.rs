//! The EU's EORI validation service (EOS, SOAP), for EU and XI EORI numbers, up to 10 per
//! request.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use roxmltree::{Document, Node};
use serde_json::{Value, json};
use ureq::http::Request;

use crate::item::{Error, Item, Live, LiveStatus, minute};
use crate::live::{Client, Reply, Source, unavailable, unreadable};

pub const EOS: Source = Source {
    name: "EOS",
    var: "VALIDATE_IDENTIFIER_EOS_URL",
    base: "https://ec.europa.eu",
    gap: Duration::from_secs(1),
};

const PATH: &str = "/taxation_customs/dds2/eos/validation/services/validation";

pub fn ask(c: &mut Client, items: &[&Item]) -> Result<Vec<Live>, Error> {
    let eoris: Vec<&str> = items.iter().filter_map(|i| i.value.as_deref()).collect();
    let list: String = eoris
        .iter()
        .map(|e| format!("<ev:eori>{e}</ev:eori>"))
        .collect();
    let body = format!(
        r#"<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/" xmlns:ev="http://eori.ws.eos.dds.s/"><soapenv:Body><ev:validateEORI>{list}</ev:validateEORI></soapenv:Body></soapenv:Envelope>"#
    );
    let url = c.url(&EOS, PATH);
    let req = || {
        Request::post(&url)
            .header("Content-Type", "text/xml; charset=utf-8")
            .header("SOAPAction", "\"\"")
            .body(body.clone().into_bytes())
            .expect("a valid request")
    };
    let results = RefCell::new(None);
    let live = c.ask(&EOS, req, |r| {
        Ok(match read(r) {
            Ok(by_eori) => {
                results.replace(Some(by_eori));
                Live::new(EOS.name, LiveStatus::Active, "")
            }
            Err(failed) => failed,
        })
    })?;
    let Some(results) = results.into_inner() else {
        return Ok(vec![live; eoris.len()]);
    };
    Ok(eoris
        .iter()
        .map(|e| match results.get(*e) {
            Some(result) => result.clone(),
            None => unreadable(&EOS, "the answer left out an EORI number"),
        })
        .collect())
}

/// The text of `n`'s child element `name`, when it has any.
fn text<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.children()
        .find(|c| c.has_tag_name(name))
        .and_then(|c| c.text())
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

/// Each EORI's answer, or what went wrong with the whole request (spec §3.6).
#[expect(clippy::result_large_err, reason = "one per request")]
fn read(r: &Reply) -> Result<HashMap<String, Live>, Live> {
    let doc = match Document::parse(&r.body) {
        Ok(doc) => doc,
        Err(_) if r.status == 200 => return Err(unreadable(&EOS, &r.body)),
        Err(_) => return Err(r.failed(&EOS)),
    };
    let root = doc.root();
    if let Some(fault) = root.descendants().find(|n| n.has_tag_name("Fault")) {
        let code = text(fault, "faultstring").unwrap_or("fault");
        return Err(Live {
            code: Some(code.into()),
            ..unavailable(
                &EOS,
                format!(
                    "EOS answered with an error ({code}). This is not a verdict on the number."
                ),
            )
        });
    }
    if r.status != 200 {
        return Err(r.failed(&EOS));
    }
    let Some(answer) = root.descendants().find(|n| n.has_tag_name("return")) else {
        return Err(unreadable(&EOS, &r.body));
    };
    if let Some(why) = text(answer, "errorDescription") {
        return Err(unavailable(
            &EOS,
            format!(
                "EOS couldn't check these numbers ({why}). This is not a verdict on the number."
            ),
        ));
    }
    let mut by_eori = HashMap::new();
    for result in answer.children().filter(|n| n.has_tag_name("result")) {
        let Some(eori) = text(result, "eori") else {
            continue;
        };
        by_eori.insert(eori.to_string(), result_live(result));
    }
    Ok(by_eori)
}

/// One EORI's `result` element as a live status.
fn result_live(result: Node) -> Live {
    let name = text(result, "name");
    let address: Vec<&str> = ["address", "street", "postalCode", "city", "country"]
        .iter()
        .filter_map(|k| text(result, k))
        .collect();
    let data = json!({
        "name": name,
        "address": (!address.is_empty()).then(|| address.join(", ")),
    });
    let now = Live::new(EOS.name, LiveStatus::Active, "");
    let (status, message, data) = match text(result, "status") {
        Some("0") => {
            let when = minute(&now.checked_at);
            let message = format!("Valid EORI number (EOS, {when}).");
            (LiveStatus::Active, message, data)
        }
        Some("1") => (
            LiveStatus::NotFound,
            "EOS has no EORI registration for this number.".into(),
            data,
        ),
        _ => {
            let why = text(result, "errorReason")
                .or(text(result, "statusDescr"))
                .unwrap_or("no status");
            let message = format!(
                "EOS couldn't check this number ({why}). This is not a verdict on the number."
            );
            (LiveStatus::Unavailable, message, Value::Null)
        }
    };
    Live {
        status,
        message,
        data,
        ..now
    }
}
