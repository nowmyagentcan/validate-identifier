use iban_validation_rs::{Iban, ValidationError};
use serde_json::json;

use crate::item::{Error, Found, a, checks};

const CHECKS: [&str; 4] = ["country", "length", "format", "check_digits"];

/// Checks a normalised (electronic form) IBAN against the IBAN registry.
pub fn check(v: &str) -> Found {
    let cc: String = v.chars().take(2).collect();
    let (failed, outcome, details) = match Iban::new(v) {
        Ok(iban) => (
            None,
            Ok("Country, length, account format and check digits pass.".to_string()),
            json!({
                "country": cc,
                "bban": &v[4..],
                "bank_id": iban.iban_bank_id,
                "branch_id": iban.iban_branch_id,
                "print": v.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c)).collect::<Vec<_>>().join(" "),
            }),
        ),
        Err(e) => {
            let (i, err) = match e {
                ValidationError::InvalidSizeForCountry => (
                    1,
                    Error::new(
                        "iban_length",
                        format!(
                            "Wrong length for {} IBAN ({} characters).",
                            a(&cc),
                            v.chars().count()
                        ),
                        Some("Check for a missing or extra character."),
                    ),
                ),
                ValidationError::StructureIncorrectForCountry => (
                    2,
                    Error::new(
                        "iban_format",
                        format!("This doesn't match the account format for {cc}."),
                        Some("Check for letters where digits belong."),
                    ),
                ),
                ValidationError::ModuloIncorrect | ValidationError::InvalidChecksum => (
                    3,
                    Error::new(
                        "iban_check_digits",
                        "The IBAN check digits don't match.",
                        Some("Check for a typo or swapped digits."),
                    ),
                ),
                ValidationError::MissingCountry
                | ValidationError::InvalidCountry
                | ValidationError::TooShort(_) => (
                    0,
                    Error::new(
                        "iban_country",
                        format!("{cc} isn't an IBAN country."),
                        Some("An IBAN starts with a country code from the IBAN registry, e.g. DE."),
                    ),
                ),
            };
            let country = (i > 0).then_some(&cc);
            let details = json!({"country": country, "bban": null, "bank_id": null, "branch_id": null, "print": null});
            (Some(i), Err(err), details)
        }
    };
    Found {
        value: Some(v.to_string()),
        country: (failed != Some(0)).then_some(cc),
        checks: checks(&CHECKS, failed),
        details,
        outcome,
        warnings: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// The account (BBAN) format of every registry country, as counts of digits (n),
    /// capital letters (a) and either (c), read from iban_validation_rs's tables.
    const FORMATS: &str = "AD8n12c AE19n AL8n16c AT16n AZ4a20c BA16n BE12n BG4a6n8c BH4a14c BI23n \
        BR8c15n2c BY4c4n16c CH5n12c CR18n CY8n16c CZ20n DE18n DJ23n DK14n DO4c20n EE16n EG25n \
        ES20n FI14n FK2a12n FO14n FR10n11c2n GP10n11c2n MQ10n11c2n GF10n11c2n RE10n11c2n \
        YT10n11c2n NC10n11c2n PF10n11c2n PM10n11c2n TF10n11c2n WF10n11c2n BL10n11c2n MF10n11c2n \
        GB4a14n IM4a14n JE4a14n GG4a14n GE2a16n GI4a15c GL14n GR7n16c GT24c HN4a20n HR17n HU24n \
        IE4a14n IL19n IQ4a15n IS22n IT1a10n12c JO4a4n18c KW4a22c KZ3n13c LB4n20c LC4a24c LI5n12c \
        LT16n LU3n13c LV4a13c LY21n MC10n11c2n MD20c ME18n MK3n10c2n MN16n MR23n MT4a5n18c \
        MU4a19n3a NI4a20n NL4a10n NO11n OM3n16c PK4a16c PL24n PS4a21c PT21n QA4a21c RO4a16c \
        RS18n RU14n15c SA2n18c SC4a20n3a SD14n SE20n SI15n SK20n SM1a10n12c SO19n ST21n SV4a20n \
        TL19n TN20n TR6n16c UA6n19c VA18n VG4a16n XK16n YE4a4n18c";

    /// An account number in `format`, varied so no two positions repeat for long.
    fn bban(format: &str) -> String {
        let (mut out, mut count) = (String::new(), 0);
        for ch in format.chars() {
            if let Some(d) = ch.to_digit(10) {
                count = count * 10 + d;
                continue;
            }
            for _ in 0..count {
                let i = out.len() as u8;
                let digit = (b'0' + i * 7 % 10) as char;
                let letter = (b'A' + i * 5 % 26) as char;
                out.push(match ch {
                    'n' => digit,
                    'a' => letter,
                    _ if i.is_multiple_of(3) => letter,
                    _ => digit,
                });
            }
            count = 0;
        }
        out
    }

    /// The IBAN for `cc` and `bban` with its check digits computed (ISO 13616).
    fn iban(cc: &str, bban: &str) -> String {
        let rem = format!("{bban}{cc}00").chars().fold(0u32, |r, c| {
            let d = c.to_digit(36).unwrap();
            (r * if d < 10 { 10 } else { 100 } + d) % 97
        });
        format!("{cc}{:02}{bban}", 98 - rem)
    }

    /// The same class of character, but a different one.
    fn change(c: u8) -> u8 {
        match c {
            b'9' => b'0',
            b'Z' => b'A',
            _ => c + 1,
        }
    }

    #[test]
    fn every_registry_country_passes_and_a_changed_character_fails() {
        let formats: Vec<_> = FORMATS.split_whitespace().collect();
        assert_eq!(formats.len(), 104);
        for f in formats {
            let (cc, format) = f.split_at(2);
            let good = iban(cc, &bban(format));
            let found = check(&good);
            assert_eq!(
                found.outcome,
                Ok("Country, length, account format and check digits pass.".into()),
                "{good}"
            );
            assert_eq!(found.country.as_deref(), Some(cc));
            for i in 2..good.len() {
                let mut bad = good.clone().into_bytes();
                bad[i] = change(bad[i]);
                let bad = String::from_utf8(bad).unwrap();
                assert_eq!(
                    check(&bad).outcome.unwrap_err().code,
                    "iban_check_digits",
                    "{bad}"
                );
            }
        }
    }

    #[test]
    fn only_registry_countries_are_known() {
        let known: Vec<_> = FORMATS.split_whitespace().map(|f| &f[..2]).collect();
        for x in b'A'..=b'Z' {
            for y in b'A'..=b'Z' {
                let cc = String::from_utf8(vec![x, y]).unwrap();
                let code = check(&format!("{cc}00")).outcome.unwrap_err().code;
                let expected = if known.contains(&cc.as_str()) {
                    "iban_length"
                } else {
                    "iban_country"
                };
                assert_eq!(code, expected, "{cc}");
            }
        }
    }

    #[test]
    fn a_valid_iban_has_its_details() {
        let gb = iban("GB", "WEST12345698765432");
        let found = check(&gb);
        assert_eq!(
            found.details,
            json!({"country": "GB", "bban": "WEST12345698765432", "bank_id": "WEST",
                   "branch_id": "123456", "print": format!("{} WEST 1234 5698 7654 32", &gb[..4])})
        );
        assert_eq!(found.checks.len(), 4);
        let de = check("DE44500105175407324931");
        assert_eq!(de.details["bank_id"], "50010517");
        assert_eq!(de.details["branch_id"], Value::Null);
        assert_eq!(de.details["print"], "DE44 5001 0517 5407 3249 31");
    }

    #[test]
    fn each_failure_gives_its_error() {
        let cases = [
            (
                "XX44500105175407324931",
                "iban_country",
                "XX isn't an IBAN country.",
                1,
            ),
            (
                "GB82WEST1234569876543",
                "iban_length",
                "Wrong length for a GB IBAN (21 characters).",
                2,
            ),
            (
                "FR763000600001123456789018",
                "iban_length",
                "Wrong length for an FR IBAN (26 characters).",
                2,
            ),
            (
                "DE445001051754073249AB",
                "iban_format",
                "This doesn't match the account format for DE.",
                3,
            ),
            (
                "DE44500105175407324932",
                "iban_check_digits",
                "The IBAN check digits don't match.",
                4,
            ),
            (
                "DE00500105175407324931",
                "iban_check_digits",
                "The IBAN check digits don't match.",
                4,
            ),
        ];
        for (v, code, message, n) in cases {
            let found = check(v);
            let e = found.outcome.unwrap_err();
            assert_eq!((e.code, e.message.as_str()), (code, message), "{v}");
            assert_eq!(found.checks.len(), n, "{v}");
            assert_eq!(found.checks[n - 1].pass, Some(false));
            assert_eq!(found.details["print"], Value::Null);
            assert_eq!(found.country.is_some(), code != "iban_country");
        }
    }
}
