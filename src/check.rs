//! One run over a list of values: the engine behind the command line, the page and the MCP
//! server, so all three give the same JSON.

use std::collections::HashMap;

use rlibphonenumber::enums::Region;
use serde::Serialize;

use crate::detect::{ambiguous, detect, found};
use crate::item::{DATA, Data, Error, Found, Item, Kind, Summary, clean};

pub const MAX_VALUES: usize = 10_000;
pub const MAX_BYTES: usize = 256;

/// `kind: None` detects each value's type.
#[derive(Default)]
pub struct Options {
    pub kind: Option<Kind>,
    pub region: Option<Region>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub results: Vec<Item>,
    pub summary: Summary,
    pub data: &'static Data,
}

pub fn usage(message: impl Into<String>, hint: &'static str) -> Error {
    Error::new("usage", message, Some(hint))
}

/// Every value's item, in input order; a repeated value is checked once.
pub fn run(values: &[String], o: &Options) -> Result<Report, Error> {
    if values.is_empty() {
        let hint = "Pass values, or a file with one value per line.";
        return Err(usage("No values to check.", hint));
    }
    if values.len() > MAX_VALUES {
        let message = format!("{} values; at most 10,000 per run.", values.len());
        return Err(usage(message, "Split the list into smaller files."));
    }
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let mut results: Vec<Item> = Vec::with_capacity(values.len());
    for v in values {
        let item = match seen.get(v.as_str()) {
            Some(&i) => Item {
                input: v.clone(),
                ..results[i].clone()
            },
            None => {
                seen.insert(v.as_str(), results.len());
                item(v, o)
            }
        };
        results.push(item);
    }
    Ok(Report {
        summary: Summary::of(&results),
        results,
        data: &DATA,
    })
}

/// One value's offline item.
pub fn item(raw: &str, o: &Options) -> Item {
    let fail = |code, message, hint| {
        Item::new(
            raw,
            None,
            Found::error(Error::new(code, message, Some(hint))),
        )
    };
    if raw.len() > MAX_BYTES {
        return fail(
            "too_long",
            "Value is longer than 256 characters.",
            "Pass one identifier per value.",
        );
    }
    if clean(raw).is_empty() {
        return fail(
            "empty",
            "Empty value.",
            "Pass an identifier, e.g. DE136695976.",
        );
    }
    let Some(kind) = o.kind.or_else(|| detect(raw, o.region)) else {
        return fail(
            "unknown_type",
            "Can't tell what kind of identifier this is.",
            "Pass --type (iban, vat, phone, email, lei, abn, uid, eori, peppol).",
        );
    };
    let mut f = found(kind, raw, o.region);
    if o.kind.is_none() {
        f.warnings.extend(ambiguous(kind, raw));
    }
    Item::new(raw, Some(kind), f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::Status;

    fn values(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn repeated_values_keep_their_own_line() {
        let r = run(
            &values(&["DE136695976", "x", "DE136695976"]),
            &Options::default(),
        )
        .unwrap();
        let inputs: Vec<_> = r.results.iter().map(|i| i.input.as_str()).collect();
        assert_eq!(inputs, ["DE136695976", "x", "DE136695976"]);
        assert_eq!(r.results[2].status, Status::Valid);
        assert_eq!(
            (r.summary.total, r.summary.valid, r.summary.invalid),
            (3, 2, 1)
        );
    }

    #[test]
    fn the_type_option_skips_detection_and_ambiguity() {
        let eori = Options {
            kind: Some(Kind::Eori),
            ..Default::default()
        };
        let i = item("DE136695976", &eori);
        assert_eq!((i.kind, i.status), (Some(Kind::Eori), Status::Valid));
        assert!(i.warnings.is_empty());
        let i = item("DE136695976", &Options::default());
        assert_eq!(i.kind, Some(Kind::Vat));
        assert_eq!(i.warnings[0].code, "ambiguous_type");
    }

    #[test]
    fn limits_hold() {
        let e = run(&[], &Options::default()).unwrap_err();
        assert_eq!(e.message, "No values to check.");
        let many = vec!["x".to_string(); MAX_VALUES + 1];
        let e = run(&many, &Options::default()).unwrap_err();
        assert_eq!(e.message, "10001 values; at most 10,000 per run.");
        assert!(run(&many[1..], &Options::default()).is_ok());
        let long = "1".repeat(MAX_BYTES + 1);
        assert_eq!(
            item(&long, &Options::default()).error.unwrap().code,
            "too_long"
        );
        assert_eq!(
            item(&long[1..], &Options::default()).error.unwrap().code,
            "unknown_type"
        );
        assert_eq!(
            item(" \u{a0}", &Options::default()).error.unwrap().code,
            "empty"
        );
    }
}
