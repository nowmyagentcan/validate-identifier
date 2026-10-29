use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Iban,
    Vat,
    Phone,
    Email,
    Lei,
    Abn,
    Uid,
    Eori,
    Peppol,
}

const PEPPOL_SCHEME: &str = "iso6523-actorid-upis::";

impl Kind {
    /// The value as this type's checks read it (spec §2.2).
    pub fn normalize(self, raw: &str) -> String {
        match self {
            Kind::Email | Kind::Phone => clean(raw),
            Kind::Peppol => {
                let v = clean(raw);
                match v.get(..PEPPOL_SCHEME.len()) {
                    Some(s) if s.eq_ignore_ascii_case(PEPPOL_SCHEME) => v[s.len()..].to_string(),
                    _ => v,
                }
            }
            Kind::Iban => {
                let v = compact(raw);
                let label = v
                    .strip_prefix("IBAN")
                    .map(|r| r.strip_prefix(':').unwrap_or(r));
                label.map_or(v.clone(), str::to_string)
            }
            Kind::Vat => {
                let v = compact(raw);
                v.strip_prefix("GR")
                    .map_or(v.clone(), |rest| format!("EL{rest}"))
            }
            _ => compact(raw),
        }
    }
}

/// Trimmed, with every Unicode space (NBSP, thin space, …) made a plain space.
pub fn clean(raw: &str) -> String {
    let space = |c: char| if c.is_whitespace() { ' ' } else { c };
    raw.trim().chars().map(space).collect()
}

/// Upper case, without spaces, dots, dashes or slashes.
pub fn compact(raw: &str) -> String {
    raw.chars()
        .filter(|&c| !c.is_whitespace() && !matches!(c, '.' | '-' | '/'))
        .flat_map(char::to_uppercase)
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Valid,
    Invalid,
    Inactive,
    NotFound,
    Unavailable,
    RateLimited,
    NotConfigured,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Checked {
    Offline,
    Live,
}

/// An input or usage error: what is wrong and, when there is one, what to do.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
    pub hint: Option<&'static str>,
}

impl Error {
    pub fn new(code: &'static str, message: impl Into<String>, hint: Option<&'static str>) -> Self {
        Error {
            code,
            message: message.into(),
            hint,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub pass: Option<bool>,
}

/// The checks named in `names`, all passed, or passed up to `failed` (which failed).
pub fn checks(names: &[&'static str], failed: Option<usize>) -> Vec<Check> {
    let end = failed.map_or(names.len(), |i| i + 1);
    names[..end]
        .iter()
        .enumerate()
        .map(|(i, &name)| Check {
            name,
            pass: Some(Some(i) != failed),
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Warning {
    pub code: &'static str,
    pub message: String,
}

/// What one type's offline checks found, before the verdict.
#[derive(Debug)]
pub struct Found {
    pub value: Option<String>,
    pub country: Option<String>,
    pub checks: Vec<Check>,
    pub details: Value,
    /// The message when every check passes, else the first failure.
    pub outcome: Result<String, Error>,
    pub warnings: Vec<Warning>,
}

impl Found {
    /// A value that failed before any type's checks ran.
    pub fn error(error: Error) -> Self {
        Found {
            value: None,
            country: None,
            checks: Vec::new(),
            details: Value::Null,
            outcome: Err(error),
            warnings: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Offline {
    pub valid: bool,
    pub checks: Vec<Check>,
}

#[derive(Debug, Serialize)]
pub struct Item {
    pub input: String,
    #[serde(rename = "type")]
    pub kind: Option<Kind>,
    pub value: Option<String>,
    pub country: Option<String>,
    pub status: Status,
    pub checked: Checked,
    pub message: String,
    pub offline: Offline,
    pub details: Value,
    pub error: Option<Error>,
    pub warnings: Vec<Warning>,
}

impl Item {
    /// The offline verdict: valid when every check passed, else invalid with the first failure.
    pub fn new(input: &str, kind: Option<Kind>, found: Found) -> Self {
        let (status, message, error) = match found.outcome {
            Ok(message) => (Status::Valid, message, None),
            Err(e) => (Status::Invalid, e.message.clone(), Some(e)),
        };
        Item {
            input: input.to_string(),
            kind,
            value: found.value,
            country: found.country,
            status,
            checked: Checked::Offline,
            message,
            offline: Offline {
                valid: status == Status::Valid,
                checks: found.checks,
            },
            details: found.details,
            error,
            warnings: found.warnings,
        }
    }
}

#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Summary {
    pub total: usize,
    pub valid: usize,
    pub invalid: usize,
    pub inactive: usize,
    pub not_found: usize,
    pub unavailable: usize,
    pub rate_limited: usize,
    pub not_configured: usize,
}

impl Summary {
    pub fn of(items: &[Item]) -> Self {
        let mut s = Summary {
            total: items.len(),
            ..Default::default()
        };
        for item in items {
            *match item.status {
                Status::Valid => &mut s.valid,
                Status::Invalid => &mut s.invalid,
                Status::Inactive => &mut s.inactive,
                Status::NotFound => &mut s.not_found,
                Status::Unavailable => &mut s.unavailable,
                Status::RateLimited => &mut s.rate_limited,
                Status::NotConfigured => &mut s.not_configured,
            } += 1;
        }
        s
    }
}

/// The data versions every result carries.
#[derive(Debug, Serialize)]
pub struct Data {
    pub version: &'static str,
    pub phone_metadata: &'static str,
    pub iban_registry: &'static str,
    pub peppol_code_list: &'static str,
}

pub const DATA: Data = Data {
    version: env!("CARGO_PKG_VERSION"),
    phone_metadata: crate::PHONE_METADATA,
    iban_registry: crate::IBAN_REGISTRY,
    peppol_code_list: crate::PEPPOL_CODE_LIST,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn values_are_normalised_per_type() {
        let n = |k: Kind, raw| k.normalize(raw);
        // NBSP, thin space and narrow NBSP count as spaces; tabs too.
        assert_eq!(
            n(
                Kind::Iban,
                " de44\u{a0}5001\u{2009}0517\u{202f}5407 3249 31\t"
            ),
            "DE44500105175407324931"
        );
        assert_eq!(
            n(Kind::Iban, "IBAN DE44 5001 0517 5407 3249 31"),
            "DE44500105175407324931"
        );
        assert_eq!(n(Kind::Iban, "iban: de44-5001"), "DE445001");
        assert_eq!(n(Kind::Vat, "de 136.695/976"), "DE136695976");
        assert_eq!(n(Kind::Vat, "GR 094 543 092"), "EL094543092");
        assert_eq!(n(Kind::Vat, "CHE-116.281.710 MWST"), "CHE116281710MWST");
        assert_eq!(n(Kind::Uid, "che-116.281.710"), "CHE116281710");
        assert_eq!(n(Kind::Abn, "51 824 753 556"), "51824753556");
        assert_eq!(n(Kind::Lei, "5493001kjtiigc8y1r12"), "5493001KJTIIGC8Y1R12");
        assert_eq!(n(Kind::Eori, "ie 8256796u"), "IE8256796U");
        assert_eq!(
            n(Kind::Phone, "\u{a0}+44 20 7946 0958 "),
            "+44 20 7946 0958"
        );
        assert_eq!(
            n(Kind::Email, " Jane.Doe@Example.COM\u{2009}"),
            "Jane.Doe@Example.COM"
        );
        // Peppol values keep their case, dashes and dots; only the scheme goes.
        assert_eq!(
            n(Kind::Peppol, "ISO6523-ACTORID-UPIS::0208:0202239951"),
            "0208:0202239951"
        );
        assert_eq!(
            n(Kind::Peppol, " 0204:991-12345.67-AB "),
            "0204:991-12345.67-AB"
        );
    }

    #[test]
    fn checks_stop_at_the_first_failure() {
        let names = ["country", "length", "format"];
        assert_eq!(checks(&names, None).len(), 3);
        let failed = checks(&names, Some(1));
        assert_eq!(failed.len(), 2);
        assert_eq!(failed[0].pass, Some(true));
        assert_eq!(
            failed[1],
            Check {
                name: "length",
                pass: Some(false)
            }
        );
    }

    #[test]
    fn a_valid_item_has_the_result_shape() {
        let found = Found {
            value: Some("DE136695976".into()),
            country: Some("DE".into()),
            checks: checks(&["format", "check_digits"], None),
            details: json!({"prefix": "DE", "number": "136695976", "branch": null}),
            outcome: Ok("Format and check digits pass.".into()),
            warnings: Vec::new(),
        };
        let item = Item::new("de 136 695 976", Some(Kind::Vat), found);
        assert_eq!(
            serde_json::to_value(&item).unwrap(),
            json!({
                "input": "de 136 695 976",
                "type": "vat",
                "value": "DE136695976",
                "country": "DE",
                "status": "valid",
                "checked": "offline",
                "message": "Format and check digits pass.",
                "offline": {"valid": true, "checks": [
                    {"name": "format", "pass": true}, {"name": "check_digits", "pass": true}
                ]},
                "details": {"prefix": "DE", "number": "136695976", "branch": null},
                "error": null,
                "warnings": []
            })
        );
    }

    #[test]
    fn an_error_makes_the_item_invalid() {
        let e = Error::new(
            "empty",
            "Empty value.",
            Some("Pass an identifier, e.g. DE136695976."),
        );
        let item = Item::new("", None, Found::error(e.clone()));
        assert_eq!(item.status, Status::Invalid);
        assert_eq!(item.message, "Empty value.");
        assert_eq!(item.error, Some(e));
        let v = serde_json::to_value(&item).unwrap();
        assert_eq!(v["type"], Value::Null);
        assert_eq!(v["offline"], json!({"valid": false, "checks": []}));
        assert_eq!(v["error"]["hint"], "Pass an identifier, e.g. DE136695976.");
    }

    #[test]
    fn summary_counts_each_status() {
        let ok = || Found {
            outcome: Ok(String::new()),
            ..Found::error(Error::new("", "", None))
        };
        let mut items = vec![
            Item::new("a", None, ok()),
            Item::new("b", None, ok()),
            Item::new(
                "c",
                None,
                Found::error(Error::new("empty", "Empty value.", None)),
            ),
        ];
        items[1].status = Status::RateLimited;
        let s = Summary::of(&items);
        assert_eq!(
            s,
            Summary {
                total: 3,
                valid: 1,
                invalid: 1,
                rate_limited: 1,
                ..Default::default()
            }
        );
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            json!({"total": 3, "valid": 1, "invalid": 1, "inactive": 0, "not_found": 0,
                   "unavailable": 0, "rate_limited": 1, "not_configured": 0})
        );
        assert_eq!(
            serde_json::to_value(&DATA).unwrap(),
            json!({"version": "0.1.0", "phone_metadata": "9.0.42", "iban_registry": "103",
                   "peppol_code_list": "9.7"})
        );
    }
}
