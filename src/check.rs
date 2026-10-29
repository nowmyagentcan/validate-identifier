//! One run over a list of values: the engine behind the command line, the page and the MCP
//! server, so all three give the same JSON.

use std::collections::HashMap;
use std::io::Write;

use rlibphonenumber::enums::Region;
use serde::Serialize;

use crate::detect::{ambiguous, detect, found};
use crate::item::{
    DATA, Data, Error, Found, Item, Kind, Live, LiveStatus, Summary, Warning, a, clean,
};
use crate::live::{Client, Source, unavailable};
use crate::source::abn::{self, ABN};
use crate::source::gleif::{self, GLEIF};
use crate::source::hmrc::{self, HMRC};
use crate::source::uid::{self, UID};
use crate::source::vies::{self, VIES};

pub const MAX_VALUES: usize = 10_000;
pub const MAX_BYTES: usize = 256;
/// Values per run with live checks: the registries are for single lookups.
pub const MAX_LIVE: usize = 1_000;

/// `kind: None` detects each value's type; `live` asks the registries, drawing a progress
/// line on stderr when `progress` is set.
#[derive(Default)]
pub struct Options {
    pub kind: Option<Kind>,
    pub region: Option<Region>,
    pub live: bool,
    pub progress: bool,
    /// Your own VAT number, sent to VIES or HMRC for a consultation number.
    pub requester: Option<String>,
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
    let requester = o.requester.as_deref().map(requester).transpose()?;
    if o.live && values.len() > MAX_LIVE {
        let message = format!(
            "{} values; at most 1,000 per run with --live.",
            values.len()
        );
        let hint = "Split the list, or check it offline first.";
        return Err(usage(message, hint));
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
    if o.live {
        live(&mut results, o.progress, requester)?;
    }
    Ok(Report {
        summary: Summary::of(&results),
        results,
        data: &DATA,
    })
}

/// `--requester` as prefix and number, when it is a valid VAT number.
fn requester(raw: &str) -> Result<(String, String), Error> {
    let f = crate::vat::check(&Kind::Vat.normalize(raw));
    let part = |k: &str| f.details[k].as_str().unwrap_or_default().to_string();
    match f.outcome {
        Ok(_) => Ok((part("prefix"), part("number"))),
        Err(_) => Err(usage(
            format!("--requester {raw} isn't a valid VAT number."),
            "Pass your own VAT number with its country prefix, e.g. DE136695976.",
        )),
    }
}

/// A registry and how to ask it about up to `batch` values (one answer per value).
struct Route {
    source: &'static Source,
    batch: usize,
    ask: fn(&mut Client, &[&Item]) -> Result<Vec<Live>, Error>,
}

const ROUTES: [Route; 5] = [
    Route {
        source: &VIES,
        batch: 1,
        ask: vies::ask,
    },
    Route {
        source: &HMRC,
        batch: 1,
        ask: hmrc::ask,
    },
    Route {
        source: &UID,
        batch: 1,
        ask: uid::ask,
    },
    Route {
        source: &ABN,
        batch: 1,
        ask: abn::ask,
    },
    Route {
        source: &GLEIF,
        batch: 100,
        ask: gleif::ask,
    },
];

/// The registry for an item whose format passed (its check digits may have failed).
fn route(i: &Item) -> Option<&'static Route> {
    let ok = i
        .error
        .as_ref()
        .is_none_or(|e| e.code.ends_with("_check_digits"));
    let digits = i.details["number"]
        .as_str()
        .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()));
    match (ok, i.kind?, i.details["prefix"].as_str()) {
        // GB numbers of the GD and HA form have no registry to ask.
        (true, Kind::Vat, Some("GB")) if digits => Some(&ROUTES[1]),
        (true, Kind::Vat, Some("CHE")) | (true, Kind::Uid, _) => Some(&ROUTES[2]),
        (true, Kind::Abn, _) => Some(&ROUTES[3]),
        (true, Kind::Lei, _) => Some(&ROUTES[4]),
        (true, Kind::Vat, Some(p)) if p != "GB" => Some(&ROUTES[0]),
        _ => None,
    }
}

/// Asks each value's registry once, one source after another in input order, and applies
/// the answers by the verdict rule. After two failures in a row a source (VIES: a country,
/// when VIES itself says it is down) is skipped for the rest of the run; a source that needs
/// credentials it doesn't have isn't asked again.
fn live(
    items: &mut [Item],
    progress: bool,
    requester: Option<(String, String)>,
) -> Result<(), Error> {
    let mut client = Client::new(&ROUTES.each_ref().map(|r| r.source))?;
    client.requester = requester;
    let mut lookups: Vec<(&Route, Vec<usize>)> = Vec::new();
    let mut seen: HashMap<(Option<Kind>, Option<&str>), usize> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        let Some(r) = route(item) else { continue };
        let key = (item.kind, item.value.as_deref());
        match seen.get(&key) {
            Some(&j) => lookups[j].1.push(i),
            None => {
                seen.insert(key, lookups.len());
                lookups.push((r, vec![i]));
            }
        }
    }
    let (total, mut done) = (lookups.len(), 0);
    let mut failed: HashMap<String, u32> = HashMap::new();
    let mut stuck: HashMap<&str, Live> = HashMap::new();
    let mut answers = Vec::with_capacity(items.len());
    for r in &ROUTES {
        let name = r.source.name;
        let mine: Vec<&Vec<usize>> = lookups
            .iter()
            .filter(|(l, _)| std::ptr::eq(*l, r))
            .map(|(_, same)| same)
            .collect();
        for chunk in mine.chunks(r.batch) {
            done += chunk.len();
            if progress {
                let dots = if crate::text::unicode(true) {
                    "…"
                } else {
                    "..."
                };
                draw(&format!("Checking {done}/{total} ({name}){dots}"));
            }
            let firsts: Vec<&Item> = chunk.iter().map(|same| &items[same[0]]).collect();
            let country = match name {
                "VIES" => format!(
                    "VIES ({})",
                    firsts[0].details["prefix"].as_str().unwrap_or("")
                ),
                _ => name.into(),
            };
            let tripped = [name.to_string(), country.clone()]
                .into_iter()
                .find(|k| failed.get(k).is_some_and(|&n| n >= 2));
            let lives = if let Some(live) = stuck.get(name) {
                vec![live.clone(); chunk.len()]
            } else if let Some(key) = tripped {
                let message = format!(
                    "Skipped: {key} failed twice in a row in this run. This is not a verdict on the number."
                );
                vec![unavailable(r.source, message); chunk.len()]
            } else {
                let lives = (r.ask)(&mut client, &firsts)?;
                let first = &lives[0];
                let own_code = first
                    .code
                    .as_deref()
                    .is_some_and(|c| c.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'));
                match first.status {
                    LiveStatus::Unavailable => {
                        let key = if own_code { country } else { name.into() };
                        *failed.entry(key).or_default() += 1;
                    }
                    LiveStatus::NotConfigured => {
                        stuck.insert(name, first.clone());
                    }
                    _ => {
                        failed.remove(name);
                        failed.remove(&country);
                    }
                }
                lives
            };
            for (same, live) in chunk.iter().zip(lives) {
                answers.extend(same.iter().map(|&i| (i, live.clone())));
            }
        }
    }
    if progress {
        draw("");
    }
    for (i, live) in answers {
        let unused = match (&client.requester, live.source) {
            (Some((p, _)), "VIES") if !vies::uses(p) => Some((a(p), "VIES")),
            (Some((p, _)), "HMRC") if p != "GB" => Some((a(p), "HMRC")),
            _ => None,
        };
        items[i].answer(live);
        if let Some((number, source)) = unused {
            items[i].warnings.push(Warning {
                code: "requester_not_used",
                message: format!("--requester is {number} number, so it wasn't sent to {source}."),
            });
        }
    }
    Ok(())
}

/// Draws `line` over the last one on stderr (empty clears it); a closed stderr loses it.
fn draw(line: &str) {
    let end = if line.is_empty() { "\r" } else { "" };
    let _ = write!(std::io::stderr(), "\r{line:<60}{end}");
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
