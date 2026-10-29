//! The Swiss UID register's public `GetByUID` (SOAP): name, UID status and VAT status in one
//! call, for Swiss VAT numbers and UIDs. Status codes are eCH-0108's.

use std::time::Duration;

use roxmltree::{Document, Node};
use serde_json::json;
use ureq::http::Request;

use crate::item::{Error, Item, Kind, Live, LiveStatus, minute};
use crate::live::{Client, Reply, Source, unavailable, unreadable};

pub const UID: Source = Source {
    name: "UID register",
    var: "VALIDATE_IDENTIFIER_UID_URL",
    base: "https://www.uid-wse.admin.ch",
    gap: Duration::from_millis(3200),
};

const PATH: &str = "/V5.0/PublicServices.svc";
const ACTION: &str = "\"http://www.uid.admin.ch/xmlns/uid-wse/IPublicServices/GetByUID\"";

pub fn ask(c: &mut Client, items: &[&Item]) -> Result<Vec<Live>, Error> {
    let item = items[0];
    let value = item.value.as_deref().unwrap_or_default();
    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
    let body = format!(
        r#"<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/" xmlns:uid="http://www.uid.admin.ch/xmlns/uid-wse" xmlns:ech="http://www.ech.ch/xmlns/eCH-0097/5"><soapenv:Body><uid:GetByUID><uid:uid><ech:uidOrganisationIdCategorie>CHE</ech:uidOrganisationIdCategorie><ech:uidOrganisationId>{digits}</ech:uidOrganisationId></uid:uid></uid:GetByUID></soapenv:Body></soapenv:Envelope>"#
    );
    let url = c.url(&UID, PATH);
    let req = || {
        Request::post(&url)
            .header("Content-Type", "text/xml; charset=utf-8")
            .header("SOAPAction", ACTION)
            .body(body.clone().into_bytes())
            .expect("a valid request")
    };
    let vat = item.kind == Some(Kind::Vat);
    Ok(vec![c.ask(&UID, req, |r| Ok(answer(r, vat)))?])
}

/// The text of the first element named `name` (any namespace) under `n`.
fn text<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.descendants()
        .find(|d| d.has_tag_name(name))
        .and_then(|d| d.text())
        .map(str::trim)
}

/// One answer as a live status (spec §3.6), for a VAT number when `vat`, else a UID.
fn answer(r: &Reply, vat: bool) -> Live {
    let Ok(doc) = Document::parse(&r.body) else {
        return match r.status {
            200 => unreadable(&UID, &r.body),
            _ => r.failed(&UID),
        };
    };
    let root = doc.root();
    if let Some(fault) = root.descendants().find(|n| n.has_tag_name("Fault")) {
        let all: String = fault.descendants().filter_map(|n| n.text()).collect();
        if all.contains("Request_limit_exceeded") {
            return Live {
                code: Some("Request_limit_exceeded".into()),
                retry_after: Some(60),
                ..Live::new(
                    UID.name,
                    LiveStatus::RateLimited,
                    "The UID register limits checks to 20 per minute and asked us to wait. Try again in a minute.",
                )
            };
        }
        let code = text(fault, "faultstring").unwrap_or("fault");
        return Live {
            code: Some(code.into()),
            ..unavailable(
                &UID,
                format!(
                    "The UID register answered with an error ({code}). This is not a verdict on the number."
                ),
            )
        };
    }
    if r.status != 200 {
        return r.failed(&UID);
    }
    let Some(result) = root
        .descendants()
        .find(|n| n.has_tag_name("GetByUIDResult"))
    else {
        return unreadable(&UID, &r.body);
    };
    if !result.children().any(|n| n.is_element()) {
        return Live::new(
            UID.name,
            LiveStatus::NotFound,
            "Not found in the public UID register.",
        );
    }
    let uid_status = match text(result, "uidregStatusEnterpriseDetail") {
        Some("1") => "provisional",
        Some("2") => "in_reactivation",
        Some("3" | "4") => "active",
        Some("5" | "6") => "deleted",
        Some("7") => "annulled",
        _ => return unreadable(&UID, &r.body),
    };
    let vat_status = text(result, "vatStatus");
    let vat_entry = text(result, "vatEntryStatus");
    let data = json!({
        "name": text(result, "organisationName"),
        "uid_status": uid_status,
        "vat_status": vat_status.map(|s| match s {
            "2" => "registered",
            "3" => "not_registered",
            _ => "unknown",
        }),
        "vat_active": match vat_entry {
            Some("1") => Some(true),
            Some("2") => Some(false),
            _ => None,
        },
        "vat_since": text(result, "vatEntryDate"),
    });
    let now = Live::new(UID.name, LiveStatus::Active, "");
    let when = minute(&now.checked_at);
    let (status, message) = match (vat, vat_status, vat_entry) {
        (false, ..) if matches!(uid_status, "deleted" | "annulled") => (
            LiveStatus::Inactive,
            format!("This UID is {uid_status} in the UID register."),
        ),
        (false, ..) => (
            LiveStatus::Active,
            format!("Registered in the UID register ({when})."),
        ),
        (true, Some("2"), Some("1")) => (
            LiveStatus::Active,
            format!("Registered for VAT (UID register, {when})."),
        ),
        (true, _, Some("2")) => (
            LiveStatus::Inactive,
            "Its VAT registration ended, says the UID register.".into(),
        ),
        (true, None | Some("3"), _) => (
            LiveStatus::NotFound,
            "Not registered for VAT, says the UID register.".into(),
        ),
        (true, ..) => (
            LiveStatus::Unavailable,
            "The UID register doesn't know this number's VAT status. This is not a verdict on the number."
                .into(),
        ),
    };
    Live {
        status,
        message,
        data,
        ..now
    }
}
