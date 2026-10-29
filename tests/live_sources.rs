//! The real registries, asked once each about identifiers they are known to hold. These need
//! the network, so they only run on request:
//! `cargo test --test live_sources -- --ignored`

mod common;

use serde_json::Value;

/// The `--json` answer for `args` checked live against the real source.
fn live(args: &[&str]) -> Vec<Value> {
    let (_, out, err) = common::vi(&[&["check", "--live", "--json"][..], args].concat(), &[]);
    let v: Value = serde_json::from_str(&out).unwrap_or_else(|_| panic!("{err}"));
    v["results"].as_array().unwrap().clone()
}

fn all_valid(args: &[&str], source: &str) -> Vec<Value> {
    let results = live(args);
    for r in &results {
        assert_eq!(r["status"], "valid", "{r}");
        assert_eq!(r["live"]["source"], source, "{r}");
    }
    results
}

#[test]
#[ignore = "real network"]
fn vies() {
    all_valid(&["IE6388047V"], "VIES");
}

#[test]
#[ignore = "real network"]
fn uid_register() {
    all_valid(&["CHE-116.281.710", "CHE-116.281.710 MWST"], "UID register");
}

#[test]
#[ignore = "real network"]
fn gleif() {
    all_valid(&["5493001KJTIIGC8Y1R12"], "GLEIF");
}

#[test]
#[ignore = "real network"]
fn eos() {
    all_valid(&["IE8256796U", "-t", "eori"], "EOS");
}

#[test]
#[ignore = "real network"]
fn hmrc_eori() {
    all_valid(&["GB220430231000", "-t", "eori"], "HMRC");
}

#[test]
#[ignore = "real network"]
fn peppol() {
    for r in all_valid(
        &["0208:0202239951", "BE0202239951", "-t", "peppol"],
        "Peppol SML",
    ) {
        let labels: Vec<&str> = r["live"]["data"]["participants"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p["document_types"].as_array())
            .flatten()
            .filter_map(|d| d["label"].as_str())
            .collect();
        for want in [
            "Invoice · Peppol BIS Billing 3.0",
            "CreditNote · Peppol BIS Billing 3.0",
        ] {
            assert!(labels.contains(&want), "{r}");
        }
    }
}

#[test]
#[ignore = "real network"]
fn mail_servers() {
    let r = all_valid(&["someone@gmail.com"], "DNS");
    assert_eq!(r[0]["live"]["data"]["kind"], "mx");
}
