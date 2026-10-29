//! Which kind of identifier a value is (the first matching rule wins), and each kind's
//! offline checks.

use iban_validation_rs::{ValidationError, validate_iban_str};
use rlibphonenumber::enums::Region;

use crate::item::{Found, Kind, Warning, clean, compact};
use crate::{email, iban, ids, peppol, phone, vat};

/// Runs `kind`'s offline checks on a raw value. Under `peppol`, a value without a colon is
/// detected and, when it is a VAT number, ABN, Swiss UID or LEI, mapped to its participant IDs.
pub fn found(kind: Kind, raw: &str, region: Option<Region>) -> Found {
    let v = kind.normalize(raw);
    match kind {
        Kind::Iban => iban::check(&v),
        Kind::Vat => vat::check(&v),
        Kind::Phone => phone::check(&v, region),
        Kind::Email => email::check(&v),
        Kind::Lei => ids::lei(&v),
        Kind::Abn => ids::abn(&v),
        Kind::Uid => ids::uid(&v),
        Kind::Eori => ids::eori(&v),
        Kind::Peppol => match detect(raw, None) {
            Some(k) if !v.contains(':') => peppol::mapped(k, found(k, raw, None)),
            _ => peppol::check(&v),
        },
    }
}

/// The kind of a raw value, or `None` when no rule fits. EORI numbers other than GB and XI
/// with 13-15 digits are never guessed: their shape also fits ordinary words.
pub fn detect(raw: &str, region: Option<Region>) -> Option<Kind> {
    let raw = clean(raw);
    let u = compact(&raw);
    let digits = |s: &str, n: std::ops::RangeInclusive<usize>| {
        n.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
    };
    let ib = Kind::Iban.normalize(&raw);
    let iban = |accept: fn(&ValidationError) -> bool| {
        let b = ib.as_bytes();
        (15..=34).contains(&b.len())
            && b[..2].iter().all(u8::is_ascii_uppercase)
            && b[2..4].iter().all(u8::is_ascii_digit)
            && b[4..]
                .iter()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            && validate_iban_str(&ib).as_ref().err().is_none_or(accept)
    };
    let p = Kind::Peppol.normalize(&raw);
    let participant = p.len() > 5
        && p.as_bytes()[..4].iter().all(u8::is_ascii_digit)
        && p.as_bytes()[4] == b':'
        && !p[5..].contains(char::is_whitespace);
    let swiss = u.strip_prefix("CHE").unwrap_or_default();
    let phone_like = !raw.is_empty()
        && raw.bytes().any(|b| b.is_ascii_digit())
        && raw
            .bytes()
            .all(|b| b.is_ascii_digit() || b" ()./-".contains(&b));
    let lei = u.len() == 20
        && u.bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
        && digits(&u[18..], 2..=2);

    Some(if raw.contains('@') {
        Kind::Email
    } else if raw.starts_with('+') {
        Kind::Phone
    } else if participant {
        Kind::Peppol
    } else if digits(swiss.get(..9).unwrap_or_default(), 9..=9)
        && ["MWST", "TVA", "IVA", "TPV"].contains(&&swiss[9..])
    {
        Kind::Vat
    } else if digits(swiss, 9..=9) {
        Kind::Uid
    } else if region.is_some() && phone_like {
        Kind::Phone
    } else if iban(|e| {
        !matches!(
            e,
            ValidationError::InvalidCountry
                | ValidationError::TooShort(_)
                | ValidationError::InvalidSizeForCountry
        )
    }) {
        Kind::Iban
    } else if lei {
        Kind::Lei
    } else if vat::format_ok(&Kind::Vat.normalize(&raw)) {
        Kind::Vat
    } else if ["GB", "XI"].contains(&u.get(..2).unwrap_or_default()) && digits(&u[2..], 13..=15) {
        Kind::Eori
    } else if digits(&u, 11..=11) && ids::abn(&u).outcome.is_ok() {
        Kind::Abn
    } else if iban(|e| !matches!(e, ValidationError::InvalidCountry)) {
        // A known IBAN country with the wrong length: the IBAN checks say what's wrong.
        Kind::Iban
    } else {
        return None;
    })
}

/// The warning for a detected value that also passes another kind's offline checks. EORI is
/// asked about only for VAT numbers, which many countries reuse as EORI numbers.
pub fn ambiguous(kind: Kind, raw: &str) -> Option<Warning> {
    let others = [
        Kind::Iban,
        Kind::Vat,
        Kind::Lei,
        Kind::Abn,
        Kind::Uid,
        Kind::Eori,
    ];
    let names: Vec<&str> = others
        .into_iter()
        .filter(|&k| k != kind && (k != Kind::Eori || kind == Kind::Vat))
        .filter(|&k| found(k, raw, None).outcome.is_ok())
        .map(Kind::name)
        .collect();
    (!names.is_empty()).then(|| Warning {
        code: "ambiguous_type",
        message: format!(
            "Also a valid {} format; pass --type to check it as one.",
            names.join(" or ")
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_follows_the_rules_in_order() {
        let gb = Some(Region::GB);
        let cases = [
            ("jane@example.com", None, Some(Kind::Email)),
            ("+44 20 7946 0958", None, Some(Kind::Phone)),
            // An @ wins over a +, a + over a participant ID.
            ("+jane@example.com", None, Some(Kind::Email)),
            ("+0208:1", None, Some(Kind::Phone)),
            ("0208:0202239951", None, Some(Kind::Peppol)),
            (
                "ISO6523-ACTORID-UPIS::0204:991-12345.67-AB",
                None,
                Some(Kind::Peppol),
            ),
            ("0208:", None, None),
            ("CHE-116.281.710 MWST", None, Some(Kind::Vat)),
            ("che116281710tva", None, Some(Kind::Vat)),
            ("CHE-116.281.710", None, Some(Kind::Uid)),
            ("020 7946 0958", gb, Some(Kind::Phone)),
            ("(020) 7946-0958", gb, Some(Kind::Phone)),
            ("020 7946 0958", None, None),
            // A region makes digit-only values phone numbers, before IBANs and ABNs.
            ("51 824 753 556", gb, Some(Kind::Phone)),
            ("DE44 5001 0517 5407 3249 31", gb, Some(Kind::Iban)),
            ("IBAN DE44500105175407324931", None, Some(Kind::Iban)),
            // Wrong check digits are still an IBAN; so is an AT IBAN that fits the LEI shape.
            ("DE44500105175407324932", None, Some(Kind::Iban)),
            ("AT611904300234573201", None, Some(Kind::Iban)),
            ("5493001KJTIIGC8Y1R12", None, Some(Kind::Lei)),
            ("DE136695976", None, Some(Kind::Vat)),
            ("GR094543092", None, Some(Kind::Vat)),
            ("de 136 695 977", None, Some(Kind::Vat)),
            ("GB220430231000", None, Some(Kind::Vat)),
            ("GB2204302310001", None, Some(Kind::Eori)),
            ("XI220430231000123", None, Some(Kind::Eori)),
            ("IE8256796U", None, Some(Kind::Vat)),
            ("51 824 753 556", None, Some(Kind::Abn)),
            ("51824753557", None, None),
            // A known IBAN country with the wrong length.
            ("GB82WEST1234569876543", None, Some(Kind::Iban)),
            ("XX82WEST12345698765432", None, None),
            ("DEADBEEF", None, None),
            ("hello", None, None),
            ("", None, None),
        ];
        for (raw, region, want) in cases {
            assert_eq!(detect(raw, region), want, "{raw}");
        }
    }

    #[test]
    fn values_of_two_kinds_say_so() {
        let w = ambiguous(Kind::Vat, "DE136695976").unwrap();
        assert_eq!(w.code, "ambiguous_type");
        assert_eq!(
            w.message,
            "Also a valid eori format; pass --type to check it as one."
        );
        // A wrong check digit doesn't stop the value being a valid EORI.
        assert!(ambiguous(Kind::Vat, "DE136695977").is_some());
        assert_eq!(
            ambiguous(Kind::Phone, "51 824 753 556").unwrap().message,
            "Also a valid abn format; pass --type to check it as one."
        );
        for (kind, raw) in [
            (Kind::Vat, "EL094543092"),
            (Kind::Iban, "DE44500105175407324931"),
            (Kind::Iban, "BE68539007547034"),
            (Kind::Lei, "5493001KJTIIGC8Y1R12"),
            (Kind::Email, "jane@example.com"),
        ] {
            assert_eq!(ambiguous(kind, raw), None, "{raw}");
        }
    }

    #[test]
    fn peppol_takes_participant_ids_or_maps_other_identifiers() {
        let candidates = |raw| found(Kind::Peppol, raw, None).details["candidates"].clone();
        assert_eq!(
            candidates("BE 0202.239.951"),
            serde_json::json!(["0208:0202239951", "9925:be0202239951"])
        );
        assert_eq!(
            candidates("iso6523-actorid-upis::0208:0202239951"),
            serde_json::json!(["0208:0202239951"])
        );
        assert_eq!(
            candidates("CHE-116.281.710"),
            serde_json::json!(["0183:che116281710"])
        );
        let code = |raw| found(Kind::Peppol, raw, None).outcome.unwrap_err().code;
        assert_eq!(code("XI434031439"), "peppol_no_scheme");
        for raw in [
            "hello",
            "DE44500105175407324931",
            "jane@example.com",
            "0208:a b",
        ] {
            assert_eq!(code(raw), "peppol_format", "{raw}");
        }
    }
}
