//! Peppol participant IDs: their syntax, the IDs other identifiers are looked up under
//! (Peppol code list 9.7), and the DNS names the SML publishes them at.

use ring::digest::{SHA256, digest};
use serde_json::json;

use crate::item::{Error, Found, Kind, checks};

const UNCHECKED: &str = "Not looked up in the Peppol SML; add --live to check registration.";

fn invalid(value: Option<String>, e: Error) -> Found {
    Found {
        value,
        checks: checks(&["format"], Some(0)),
        ..Found::error(e)
    }
}

fn format_error() -> Error {
    Error::new(
        "peppol_format",
        "A Peppol participant ID looks like 0208:0202239951 (4-digit scheme code, colon, value).",
        Some("Or pass a VAT number, ABN, Swiss UID or LEI with --type peppol."),
    )
}

/// Checks `ICD:value` (scheme already dropped): 4 digits, a colon, and a value without spaces
/// or control characters, kept as given.
pub fn check(v: &str) -> Found {
    let ok = v.split_once(':').is_some_and(|(icd, value)| {
        icd.len() == 4
            && icd.bytes().all(|b| b.is_ascii_digit())
            && !value.is_empty()
            && !value.chars().any(|c| c.is_whitespace() || c.is_control())
    });
    if !ok {
        return invalid(Some(v.into()), format_error());
    }
    Found {
        value: Some(v.into()),
        country: None,
        checks: checks(&["format"], None),
        details: json!({ "candidates": [v] }),
        outcome: Ok(format!("Participant ID format passes. {UNCHECKED}")),
        warnings: Vec::new(),
    }
}

/// The Peppol participant IDs of a VAT number, ABN, Swiss UID or LEI whose own checks are in
/// `f`; anything else, or one that failed its checks, is a `peppol_format` error.
pub fn mapped(kind: Kind, f: Found) -> Found {
    if f.outcome.is_err() {
        return invalid(f.value, format_error());
    }
    let value = f.value.clone().unwrap_or_default();
    let candidates = match kind {
        Kind::Vat => {
            let prefix = f.details["prefix"].as_str().unwrap_or_default();
            let number = f.details["number"].as_str().unwrap_or_default();
            vat_ids(prefix, number)
        }
        Kind::Abn => vec![format!("0151:{value}")],
        Kind::Uid => vec![format!("0183:che{}", value[4..].replace('.', ""))],
        Kind::Lei => vec![format!("0199:{value}")],
        _ => return invalid(f.value, format_error()),
    };
    if candidates.is_empty() {
        let e = Error::new(
            "peppol_no_scheme",
            format!("{} VAT numbers have no Peppol scheme.", &value[..2]),
            Some("Pass the participant ID, e.g. 0088:<GLN>."),
        );
        return invalid(f.value, e);
    }
    Found {
        country: None,
        details: json!({ "candidates": candidates }),
        outcome: Ok(format!(
            "Peppol IDs: {}. {UNCHECKED}",
            candidates.join(" and ")
        )),
        ..f
    }
}

/// The countries whose VAT scheme in code list 9.7 is the prefix and number under a 99xx code.
const SCHEMES: &str = "AT9914 BG9926 CY9928 CZ9929 DE9930 EE9931 ES9920 FR9957 GB9932 HR9934     HU9910 IE9935 LT9937 LU9938 LV9939 MT9943 NL9944 PL9945 PT9946 RO9947 SI9949 SK9950";

/// The participant IDs of a VAT number: mostly the prefix and number, lower case, under the
/// country's 99xx code; a few countries use another register's number.
fn vat_ids(prefix: &str, n: &str) -> Vec<String> {
    match prefix {
        "BE" => vec![format!("0208:{n}"), format!("9925:be{n}")],
        "EL" => vec![format!("9933:el{n}"), format!("9933:gr{n}")],
        "CHE" => vec![format!("0183:che{n}"), format!("9927:che{n}")],
        "IT" => vec![format!("0211:it{n}")],
        "DK" => vec![format!("0184:{n}")],
        "SE" => vec![format!("0007:{}", &n[..10])],
        "FI" => vec![format!("0216:0037{n}")],
        _ => SCHEMES
            .split_whitespace()
            .find_map(|s| s.strip_prefix(prefix))
            .map(|icd| format!("{icd}:{prefix}{n}").to_ascii_lowercase())
            .into_iter()
            .collect(),
    }
}

/// The DNS name the SML publishes a participant at: base32 of the SHA-256 of the lower-cased
/// `ICD:value` (Peppol Policy for use of Identifiers 4.4.0, POLICY 7).
pub fn sml_name(id: &str, zone: &str) -> String {
    let hash = digest(&SHA256, id.to_ascii_lowercase().as_bytes());
    format!("{}.iso6523-actorid-upis.{zone}", base32(hash.as_ref()))
}

/// RFC 4648 base32 without padding.
fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let (mut out, mut buf, mut bits) = (String::new(), 0u32, 0);
    for &b in bytes {
        buf = (buf << 8 | u32::from(b)) & 0xfff;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[(buf >> bits & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[(buf << (5 - bits) & 31) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ids, vat};

    fn candidates(f: &Found) -> Vec<&str> {
        let list = f.details["candidates"].as_array().unwrap();
        list.iter().map(|c| c.as_str().unwrap()).collect()
    }

    #[test]
    fn participant_ids_have_a_scheme_code_and_a_value() {
        for v in [
            "0208:0202239951",
            "0204:991-12345.67-AB",
            "9930:de136695976",
        ] {
            let f = check(v);
            assert_eq!(candidates(&f), [v]);
            assert_eq!(f.value.as_deref(), Some(v));
            assert_eq!(
                f.outcome.unwrap(),
                "Participant ID format passes. Not looked up in the Peppol SML; add --live to check registration."
            );
        }
        for bad in [
            "208:1",
            "02081:1",
            "02a8:1",
            "0208:",
            "0208",
            "0208:a b",
            "0208:a\u{1}",
        ] {
            let e = check(bad).outcome.unwrap_err();
            assert_eq!(e.code, "peppol_format", "{bad}");
            assert_eq!(
                e.message,
                "A Peppol participant ID looks like 0208:0202239951 (4-digit scheme code, colon, value)."
            );
        }
    }

    #[test]
    fn other_identifiers_map_to_their_schemes() {
        let vat_cases = [
            (
                "BE0202239951",
                &["0208:0202239951", "9925:be0202239951"][..],
            ),
            ("EL094543092", &["9933:el094543092", "9933:gr094543092"]),
            (
                "CHE116281710MWST",
                &["0183:che116281710", "9927:che116281710"],
            ),
            ("DE136695976", &["9930:de136695976"]),
            ("IT00197200132", &["0211:it00197200132"]),
            ("DK61126228", &["0184:61126228"]),
            ("SE556464687401", &["0007:5564646874"]),
            ("FI20584306", &["0216:003720584306"]),
            ("ATU66059506", &["9914:atu66059506"]),
        ];
        for (v, want) in vat_cases {
            let f = mapped(Kind::Vat, vat::check(v));
            assert_eq!(candidates(&f), want, "{v}");
            assert_eq!(f.country, None);
        }
        let f = mapped(Kind::Vat, vat::check("BE0202239951"));
        assert_eq!(f.value.as_deref(), Some("BE0202239951"));
        assert_eq!(
            f.outcome.unwrap(),
            "Peppol IDs: 0208:0202239951 and 9925:be0202239951. Not looked up in the Peppol SML; add --live to check registration."
        );
        let others = [
            (Kind::Abn, ids::abn("51824753556"), "0151:51824753556"),
            (Kind::Uid, ids::uid("CHE116281710"), "0183:che116281710"),
            (
                Kind::Lei,
                ids::lei("5493001KJTIIGC8Y1R12"),
                "0199:5493001KJTIIGC8Y1R12",
            ),
        ];
        for (kind, found, want) in others {
            assert_eq!(candidates(&mapped(kind, found)), [want]);
        }
    }

    #[test]
    fn some_values_have_no_participant_id() {
        let e = mapped(Kind::Vat, vat::check("XI434031439"))
            .outcome
            .unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.hint),
            (
                "peppol_no_scheme",
                "XI VAT numbers have no Peppol scheme.",
                Some("Pass the participant ID, e.g. 0088:<GLN>.")
            )
        );
        for (kind, f) in [
            (Kind::Vat, vat::check("DE136695977")),
            (Kind::Eori, ids::eori("IE8256796U")),
        ] {
            assert_eq!(mapped(kind, f).outcome.unwrap_err().code, "peppol_format");
        }
    }

    #[test]
    fn sml_names_are_base32_sha256() {
        // RFC 4648 test vectors, without padding.
        for (input, want) in [
            ("", ""),
            ("f", "MY"),
            ("fo", "MZXQ"),
            ("foo", "MZXW6"),
            ("foob", "MZXW6YQ"),
            ("fooba", "MZXW6YTB"),
            ("foobar", "MZXW6YTBOI"),
        ] {
            assert_eq!(base32(input.as_bytes()), want);
        }
        let zone = "participant.sml.prod.tech.peppol.org";
        let name = sml_name("0208:0202239951", zone);
        assert!(name.starts_with("FERPW6OV"), "{name}");
        assert_eq!(name.len(), 52 + ".iso6523-actorid-upis.".len() + zone.len());
        assert!(name.ends_with(".iso6523-actorid-upis.participant.sml.prod.tech.peppol.org"));
        assert_eq!(
            sml_name("9925:BE0202239951", zone),
            sml_name("9925:be0202239951", zone)
        );
    }
}
