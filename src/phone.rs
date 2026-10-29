use rlibphonenumber::{
    NumberLengthType, PHONE_NUMBER_UTIL, ParseError, PhoneNumber, PhoneNumberFormat as Format,
    PhoneNumberType as Type, enums::Region,
};
use serde_json::json;

use crate::item::{Error, Found, checks};

const CHECKS: [&str; 3] = ["format", "length", "number"];

/// Checks a phone number in `+` form, or in national form for `region`.
/// Validity comes from `is_valid` and `get_region_code`, never `is_valid_number_for_region`,
/// which accepts other countries' numbers (rlibphonenumber PR #35).
pub fn check(v: &str, region: Option<Region>) -> Found {
    let fail = |code, message: &str, hint| Found {
        checks: checks(&CHECKS, Some(0)),
        ..Found::error(Error::new(code, message, Some(hint)))
    };
    if !v.starts_with('+') && region.is_none() {
        let message = "A phone number without + needs a region.";
        return fail(
            "phone_region",
            message,
            "Add --region GB, or write it as +44….",
        );
    }
    let n = match PhoneNumber::parse(v, region) {
        Ok(n) => n,
        Err(e) => {
            let code: String = v
                .chars()
                .skip(1)
                .take_while(char::is_ascii_digit)
                .take(3)
                .collect();
            let message = match e {
                ParseError::InvalidCountryCode if !code.is_empty() => {
                    format!("+{code} isn't a country calling code.")
                }
                ParseError::TooShortAfterIdd | ParseError::TooShortNsn => {
                    "Too short to be a phone number.".into()
                }
                ParseError::TooLongNsn => "Too long to be a phone number.".into(),
                _ => "This isn't a phone number.".into(),
            };
            let hint = "Write it in international form, e.g. +44 20 7946 0958.";
            return fail("phone_parse", &message, hint);
        }
    };

    let geographic = |r: Region| (r != Region::World).then_some(r.to_string());
    let region = n
        .get_region_code()
        .or_else(|| PHONE_NUMBER_UTIL.get_region_for_country_code(n.country_code))
        .and_then(geographic);
    let place = |prep: &str| match &region {
        Some(r) => format!("{prep} {r}"),
        None => format!("for +{}", n.country_code),
    };
    let international = n.format_as(Format::International).into_owned();
    let possible = n.is_possible_with_reason() == Ok(NumberLengthType::IsPossible);
    let (code, words) = number_type(n.get_type());
    let (failed, outcome) = if !possible {
        (
            Some(1),
            Err(format!(
                "{international} isn't a valid number {}.",
                place("in")
            )),
        )
    } else if !n.is_valid() {
        let place = place("for");
        let message =
            format!("{international} has the right length {place} but isn't in an assigned range.");
        (Some(2), Err(message))
    } else {
        (
            None,
            Ok(format!(
                "Valid {words} number{}.",
                region
                    .as_ref()
                    .map_or(String::new(), |r| format!(" in {r}"))
            )),
        )
    };
    Found {
        value: Some(n.format_as(Format::E164).into_owned()),
        country: region.clone(),
        checks: checks(&CHECKS, failed),
        details: json!({
            "e164": n.format_as(Format::E164),
            "international": international,
            "national": n.format_as(Format::National),
            "region": region,
            "country_code": n.country_code,
            "number_type": code,
            "possible": possible,
            "extension": n.extension,
        }),
        outcome: outcome.map_err(|m| {
            Error::new(
                "phone_invalid",
                m,
                Some("Check the number of digits and the area code."),
            )
        }),
        warnings: Vec::new(),
    }
}

/// The JSON name of a number type and its words for messages.
fn number_type(t: Type) -> (&'static str, &'static str) {
    match t {
        Type::FixedLine => ("fixed_line", "fixed line"),
        Type::Mobile => ("mobile", "mobile"),
        Type::FixedLineOrMobile => ("fixed_line_or_mobile", "fixed line or mobile"),
        Type::TollFree => ("toll_free", "toll free"),
        Type::PremiumRate => ("premium_rate", "premium rate"),
        Type::SharedCost => ("shared_cost", "shared cost"),
        Type::VoIP => ("voip", "VoIP"),
        Type::PersonalNumber => ("personal_number", "personal"),
        Type::Pager => ("pager", "pager"),
        Type::UAN => ("uan", "UAN"),
        Type::VoiceMail => ("voicemail", "voicemail"),
        Type::Unknown => ("unknown", "unknown type"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn region(code: &str) -> Option<Region> {
        Some(Region::from_code(code).unwrap())
    }

    #[test]
    fn a_valid_number_has_its_formats_and_type() {
        let found = check("+44 20 7946 0958", None);
        assert_eq!(found.outcome, Ok("Valid fixed line number in GB.".into()));
        assert_eq!(found.value.as_deref(), Some("+442079460958"));
        assert_eq!(found.country.as_deref(), Some("GB"));
        assert_eq!(
            found.details,
            json!({"e164": "+442079460958", "international": "+44 20 7946 0958",
                   "national": "020 7946 0958", "region": "GB", "country_code": 44,
                   "number_type": "fixed_line", "possible": true, "extension": null})
        );
        assert_eq!(found.checks.len(), 3);
    }

    #[test]
    fn a_national_number_takes_the_region_of_its_range() {
        let found = check("07911 123456", region("GB"));
        assert_eq!(found.details["region"], "GG");
        assert_eq!(found.country.as_deref(), Some("GG"));
        assert_eq!(found.value.as_deref(), Some("+447911123456"));
    }

    #[test]
    fn extensions_and_non_geographic_numbers() {
        for v in ["+44 20 7946 0958 ext. 123", "+44 20 7946 0958 x123"] {
            let found = check(v, None);
            assert!(found.outcome.is_ok(), "{v}");
            assert_eq!(found.details["extension"], "123", "{v}");
        }
        let toll_free = PHONE_NUMBER_UTIL
            .get_example_number_for_non_geo_entity(800)
            .unwrap();
        let found = check(&toll_free.format_as(Format::E164), None);
        assert_eq!(found.outcome, Ok("Valid toll free number.".into()));
        assert_eq!(found.details["region"], Value::Null);
        assert_eq!(found.details["country_code"], 800);
        assert_eq!(found.country, None);
    }

    #[test]
    fn each_failure_gives_its_error() {
        let cases = [
            (
                "020 7946 0958",
                None,
                "phone_region",
                "A phone number without + needs a region.",
            ),
            (
                "+999 1234 5678",
                None,
                "phone_parse",
                "+999 isn't a country calling code.",
            ),
            ("+hello", None, "phone_parse", "This isn't a phone number."),
            (
                "hello",
                region("GB"),
                "phone_parse",
                "This isn't a phone number.",
            ),
            (
                "+44 1",
                None,
                "phone_parse",
                "Too short to be a phone number.",
            ),
            (
                "+44 1234 5678 9012 3456 7890",
                None,
                "phone_parse",
                "Too long to be a phone number.",
            ),
            (
                "+44 1234",
                None,
                "phone_invalid",
                "+44 1234 isn't a valid number in GB.",
            ),
            (
                "+44 1632 960000",
                None,
                "phone_invalid",
                "+44 1632 960000 has the right length for GB but isn't in an assigned range.",
            ),
        ];
        for (v, r, code, message) in cases {
            let found = check(v, r);
            let e = found.outcome.expect_err(v);
            assert_eq!((e.code, e.message.as_str()), (code, message), "{v}");
            assert_eq!(found.checks.last().unwrap().pass, Some(false), "{v}");
        }
        // Ofcom keeps 01632 960xxx for drama: the right length, never assigned.
        let unassigned = check("+44 1632 960000", None);
        assert_eq!(unassigned.details["possible"], true);
        assert_eq!(unassigned.details["number_type"], "unknown");
        assert_eq!(unassigned.country.as_deref(), Some("GB"));
    }

    #[test]
    fn google_example_numbers_pass_for_20_regions() {
        let regions = [
            "US", "GB", "DE", "FR", "IT", "ES", "NL", "BE", "CH", "AT", "SE", "PL", "IE", "AU",
            "CA", "JP", "IN", "BR", "ZA", "SG",
        ];
        for r in regions {
            let example = PHONE_NUMBER_UTIL
                .get_example_number(region(r).unwrap())
                .unwrap();
            let e164 = example.format_as(Format::E164);
            let found = check(&e164, None);
            assert!(found.outcome.is_ok(), "{r} {e164}");
            assert_eq!(found.details["region"], r, "{e164}");
            assert_eq!(found.value.as_deref(), Some(&*e164));
            assert_eq!(
                found.details["number_type"],
                number_type(example.get_type()).0,
                "{e164}"
            );
            // The national form reads back to the same number.
            let national = found.details["national"].as_str().unwrap().to_string();
            assert_eq!(
                check(&national, region(r)).value.as_deref(),
                Some(&*e164),
                "{r}"
            );
        }
    }

    #[test]
    fn every_number_type_is_named() {
        let types = [
            Type::FixedLine,
            Type::Mobile,
            Type::TollFree,
            Type::PremiumRate,
            Type::SharedCost,
            Type::VoIP,
            Type::PersonalNumber,
            Type::Pager,
            Type::UAN,
            Type::VoiceMail,
        ];
        for t in types {
            let example = PHONE_NUMBER_UTIL.get_example_number_for_type(t).unwrap();
            let found = check(&example.format_as(Format::E164), None);
            assert!(found.outcome.is_ok(), "{t:?}");
            assert_eq!(
                found.details["number_type"],
                number_type(example.get_type()).0
            );
            assert_ne!(found.details["number_type"], "unknown");
        }
        let us = check("+1 201 555 0123", None);
        assert_eq!(us.details["number_type"], "fixed_line_or_mobile");
        assert_eq!(
            us.outcome,
            Ok("Valid fixed line or mobile number in US.".into())
        );
    }
}
