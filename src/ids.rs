//! LEI (ISO 17442), Australian ABN, Swiss UID (eCH-0097) and EORI numbers.

use serde_json::json;

use crate::item::{Error, Found, checks};

const DIGITS: [&str; 2] = ["format", "check_digits"];

/// A number whose format and check digits passed, as `value` in `country`.
fn passed(value: String, country: Option<&str>, details: serde_json::Value, msg: String) -> Found {
    Found {
        value: Some(value),
        country: country.map(str::to_string),
        checks: checks(&DIGITS, None),
        details,
        outcome: Ok(msg),
        warnings: Vec::new(),
    }
}

fn failed(v: &str, country: Option<&str>, i: usize, e: Error) -> Found {
    Found {
        value: Some(v.to_string()),
        country: country.map(str::to_string),
        checks: checks(&DIGITS, Some(i)),
        details: json!({}),
        ..Found::error(e)
    }
}

fn unchecked(source: &str) -> String {
    format!(
        "Format and check digits pass. Not checked against {source}; add --live to confirm registration."
    )
}

/// ISO 17442: 18 letters or digits, then 2 check digits; ISO 7064 MOD 97-10 of the whole is 1.
pub fn lei(v: &str) -> Found {
    let alnum = |b: &u8| b.is_ascii_digit() || b.is_ascii_uppercase();
    let b = v.as_bytes();
    if b.len() != 20 || !b[..18].iter().all(alnum) || !b[18..].iter().all(u8::is_ascii_digit) {
        let e = Error::new(
            "lei_format",
            "An LEI is 18 letters or digits followed by 2 check digits.",
            None,
        );
        return failed(v, None, 0, e);
    }
    let rem = b.iter().fold(0u32, |r, &c| match c {
        b'0'..=b'9' => (r * 10 + u32::from(c - b'0')) % 97,
        _ => (r * 100 + u32::from(c - b'A') + 10) % 97,
    });
    if rem != 1 {
        let hint = Some("Check for a typo, or add --live to ask GLEIF.");
        let e = Error::new(
            "lei_check_digits",
            "The LEI check digits don't match.",
            hint,
        );
        return failed(v, None, 1, e);
    }
    passed(v.into(), None, json!({}), unchecked("GLEIF"))
}

/// 11 digits; with 1 taken from the first, the weighted sum is a multiple of 89.
pub fn abn(v: &str) -> Found {
    if v.len() != 11 || !v.bytes().all(|b| b.is_ascii_digit()) {
        return failed(
            v,
            Some("AU"),
            0,
            Error::new("abn_format", "An ABN is 11 digits.", None),
        );
    }
    const WEIGHTS: [i32; 11] = [10, 1, 3, 5, 7, 9, 11, 13, 15, 17, 19];
    let sum: i32 = v
        .bytes()
        .zip(WEIGHTS)
        .enumerate()
        .map(|(i, (b, w))| (i32::from(b - b'0') - i32::from(i == 0)) * w)
        .sum();
    if sum % 89 != 0 {
        let hint = Some("Check for a typo, or add --live to ask ABN Lookup.");
        let e = Error::new(
            "abn_check_digits",
            "The ABN check digits don't match.",
            hint,
        );
        return failed(v, Some("AU"), 1, e);
    }
    passed(v.into(), Some("AU"), json!({}), unchecked("ABN Lookup"))
}

/// `CHE` and 9 digits, the last a mod 11 check digit (eCH-0097); shown as `CHE-116.281.710`.
pub fn uid(v: &str) -> Found {
    let d = v
        .strip_prefix("CHE")
        .filter(|d| d.len() == 9 && d.bytes().all(|b| b.is_ascii_digit()));
    let Some(d) = d else {
        let e = Error::new(
            "uid_format",
            "A Swiss UID is CHE followed by 9 digits.",
            Some("e.g. CHE-116.281.710"),
        );
        return failed(v, Some("CH"), 0, e);
    };
    let figs: Vec<u32> = d.bytes().map(|b| u32::from(b - b'0')).collect();
    let check = crate::vat::uid_check_digit(&figs[..8]);
    if check != figs[8] {
        let message = match check {
            10 => "No UID has this number; its check digit would be 10.",
            _ => "The UID check digit doesn't match.",
        };
        return failed(
            v,
            Some("CH"),
            1,
            Error::new("uid_check_digits", message, None),
        );
    }
    let display = format!("CHE-{}.{}.{}", &d[..3], &d[3..6], &d[6..]);
    let msg = "Format and check digit pass. Not checked against the UID register; add --live to confirm registration.";
    passed(
        display.clone(),
        Some("CH"),
        json!({"display": display}),
        msg.into(),
    )
}

/// The countries that issue EORI numbers: the EU 27 by ISO code, Northern Ireland and the UK.
const EORI_COUNTRIES: [&str; 29] = [
    "AT", "BE", "BG", "CY", "CZ", "DE", "DK", "EE", "ES", "FI", "FR", "GR", "HR", "HU", "IE", "IT",
    "LT", "LU", "LV", "MT", "NL", "PL", "PT", "RO", "SE", "SI", "SK", "XI", "GB",
];

/// A country code and 1-15 letters or digits (GB and XI: 12-15 digits). No check digit exists.
pub fn eori(v: &str) -> Found {
    const CHECKS: [&str; 2] = ["country", "format"];
    let fail = |i, country: Option<&str>, e| Found {
        checks: checks(&CHECKS, Some(i)),
        ..failed(v, country, 0, e)
    };
    let generic = "An EORI is a country code followed by up to 15 letters or digits.";
    let cc = v
        .get(..2)
        .filter(|c| c.bytes().all(|b| b.is_ascii_uppercase()));
    let Some(cc) = cc else {
        return fail(0, None, Error::new("eori_format", generic, None));
    };
    if !EORI_COUNTRIES.contains(&cc) {
        let e = Error::new(
            "eori_country",
            format!("{cc} isn't an EU country, GB or XI."),
            None,
        );
        return fail(0, None, e);
    }
    let rest = &v[2..];
    let (ok, message) = match cc {
        "GB" | "XI" => (
            (12..=15).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_digit()),
            format!(
                "{} EORI is {cc} followed by 12 to 15 digits.",
                if cc == "XI" { "An XI" } else { "A GB" }
            ),
        ),
        _ => (
            (1..=15).contains(&rest.len())
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase()),
            generic.into(),
        ),
    };
    if !ok {
        return fail(1, Some(cc), Error::new("eori_format", message, None));
    }
    Found {
        value: Some(v.into()),
        country: Some(cc.into()),
        checks: checks(&CHECKS, None),
        details: json!({}),
        outcome: Ok("Format only: EORI numbers have no check digit. Add --live to confirm.".into()),
        warnings: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::Kind;

    fn error(f: &Found) -> (&str, &str) {
        let e = f.outcome.as_ref().unwrap_err();
        (e.code, e.message.as_str())
    }

    #[test]
    fn leis_pass_mod_97_10() {
        for v in [
            "5493001KJTIIGC8Y1R12",
            "529900T8BM49AURSDO55",
            "HWUPKR0MPOU8FGXBT394",
        ] {
            let f = lei(v);
            assert!(f.outcome.is_ok(), "{v}");
            assert_eq!((f.value.as_deref(), f.country), (Some(v), None));
            assert_eq!(f.details, json!({}));
        }
        assert_eq!(
            error(&lei("5493001KJTIIGC8Y1R13")),
            ("lei_check_digits", "The LEI check digits don't match.")
        );
        // Positions 5-6 need not be 00.
        assert!(lei("2138001WXZQOPMPA3D50").outcome.is_ok());
        for bad in [
            "5493001KJTIIGC8Y1R1",
            "5493001KJTIIGC8Y1RAB",
            "5493001kjtiigc8y1r12",
        ] {
            assert_eq!(error(&lei(bad)).0, "lei_format", "{bad}");
        }
        // Changing any one digit to another digit, or letter to letter, fails.
        let v = "5493001KJTIIGC8Y1R12";
        for i in 0..20 {
            let mut b = v.as_bytes().to_vec();
            b[i] = match b[i] {
                b'9' => b'0',
                b'Z' => b'A',
                c => c + 1,
            };
            assert!(
                lei(std::str::from_utf8(&b).unwrap()).outcome.is_err(),
                "{i}"
            );
        }
    }

    #[test]
    fn abns_pass_mod_89() {
        let f = abn(&Kind::Abn.normalize("51 824 753 556"));
        assert_eq!(f.value.as_deref(), Some("51824753556"));
        assert_eq!(f.country.as_deref(), Some("AU"));
        assert_eq!(
            f.outcome.unwrap(),
            "Format and check digits pass. Not checked against ABN Lookup; add --live to confirm registration."
        );
        assert!(abn("53004085616").outcome.is_ok());
        assert_eq!(
            error(&abn("51824753557")),
            ("abn_check_digits", "The ABN check digits don't match.")
        );
        for bad in ["5182475355", "5182475355A", "518247535560"] {
            assert_eq!(
                error(&abn(bad)),
                ("abn_format", "An ABN is 11 digits."),
                "{bad}"
            );
        }
    }

    #[test]
    fn uids_pass_mod_11() {
        for (v, display) in [
            ("CHE116281710", "CHE-116.281.710"),
            ("CHE100000006", "CHE-100.000.006"),
        ] {
            let f = uid(v);
            assert!(f.outcome.is_ok(), "{v}");
            assert_eq!(f.value.as_deref(), Some(display));
            assert_eq!(f.details, json!({"display": display}));
            assert_eq!(f.country.as_deref(), Some("CH"));
        }
        assert_eq!(
            error(&uid("CHE116281711")),
            ("uid_check_digits", "The UID check digit doesn't match.")
        );
        // Weighted sum 45 leaves 1 mod 11, so the check would be 10.
        assert_eq!(
            error(&uid("CHE100000800")),
            (
                "uid_check_digits",
                "No UID has this number; its check digit would be 10."
            )
        );
        for bad in ["CHE11628171", "CH116281710", "CHE11628171X"] {
            assert_eq!(error(&uid(bad)).0, "uid_format", "{bad}");
        }
    }

    #[test]
    fn eoris_have_a_country_and_a_format() {
        for (v, cc) in [
            ("IE8256796U", "IE"),
            ("GB220430231000", "GB"),
            ("XI123456789000", "XI"),
            ("DE1", "DE"),
        ] {
            let f = eori(v);
            assert_eq!(f.country.as_deref(), Some(cc));
            assert_eq!(
                f.outcome.unwrap(),
                "Format only: EORI numbers have no check digit. Add --live to confirm."
            );
        }
        assert_eq!(
            error(&eori("EL123")),
            ("eori_country", "EL isn't an EU country, GB or XI.")
        );
        assert_eq!(
            error(&eori("GB12345678901")),
            (
                "eori_format",
                "A GB EORI is GB followed by 12 to 15 digits."
            )
        );
        assert_eq!(
            error(&eori("XI12345678901A")),
            (
                "eori_format",
                "An XI EORI is XI followed by 12 to 15 digits."
            )
        );
        let generic = "An EORI is a country code followed by up to 15 letters or digits.";
        for bad in ["DE", "DE1234567890123456", "12345", "D"] {
            assert_eq!(error(&eori(bad)), ("eori_format", generic), "{bad}");
        }
    }
}
