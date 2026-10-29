//! Text for people. Styles are always written as ANSI codes; the output stream drops them
//! unless it is a colour terminal and NO_COLOR is unset.

use std::fmt::Write;

use crate::check::Report;
use crate::item::{Error, Item, Kind, Status, Summary, minute};

/// What the tool is for, in one sentence: the help's first line.
pub const SENTENCE: &str = "Check VAT numbers, IBANs, phone numbers, emails, LEIs and Peppol IDs.";

pub const BOLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
pub const RESET: &str = "\x1b[0m";

/// Shown values longer than this push their own line instead of padding every other.
const PAD_LIMIT: usize = 34;

fn shown(i: &Item) -> &str {
    i.value.as_deref().unwrap_or(&i.input)
}

/// `IBAN`, `Swiss UID`, `Unknown`.
pub fn type_name(kind: Option<Kind>) -> &'static str {
    match kind {
        Some(Kind::Iban) => "IBAN",
        Some(Kind::Vat) => "VAT",
        Some(Kind::Phone) => "Phone",
        Some(Kind::Email) => "Email",
        Some(Kind::Lei) => "LEI",
        Some(Kind::Abn) => "ABN",
        Some(Kind::Uid) => "Swiss UID",
        Some(Kind::Eori) => "EORI",
        Some(Kind::Peppol) => "Peppol",
        None => "Unknown",
    }
}

/// `IBAN · DE`, `Swiss UID · CH`, `Peppol`, `Unknown`.
fn label(i: &Item) -> String {
    let name = type_name(i.kind);
    match &i.country {
        Some(c) => format!("{name} · {c}"),
        None => name.into(),
    }
}

/// The mark, the verdict in words and their colour.
fn verdict(s: Status) -> (&'static str, &'static str, &'static str) {
    match s {
        Status::Valid => ("✓", "valid", GREEN),
        Status::Invalid => ("✗", "invalid", RED),
        Status::Inactive => ("✗", "inactive", RED),
        Status::NotFound => ("✗", "not found", RED),
        Status::Unavailable => ("!", "unavailable", YELLOW),
        Status::RateLimited => ("!", "rate limited", YELLOW),
        Status::NotConfigured => ("!", "not configured", YELLOW),
    }
}

/// What follows the verdict: the facts a valid value has, then the message when it adds one.
fn facts(i: &Item) -> String {
    let d = &i.details;
    let mut facts: Vec<String> = Vec::new();
    if let Some(live) = i.live.as_ref().filter(|l| l.answered()) {
        facts.push(format!("{} {}", live.source, minute(&live.checked_at)));
        let name = ["name", "entity_name", "legal_name"]
            .iter()
            .find_map(|k| live.data[k].as_str());
        facts.extend(name.map(String::from));
    }
    if i.status == Status::Valid {
        match i.kind {
            Some(Kind::Iban) => facts.push(d["print"].as_str().unwrap_or_default().into()),
            Some(Kind::Phone) => {
                let kind = d["number_type"].as_str().unwrap_or_default();
                facts.push(kind.replace('_', " "));
                facts.push(d["international"].as_str().unwrap_or_default().into());
            }
            _ => {}
        }
    }
    if i.status != Status::Valid || facts.is_empty() {
        facts.push(i.message.clone());
    }
    facts.iter().map(|f| format!(" · {f}")).collect()
}

/// The labels of the document types a Peppol participant accepts.
fn document_labels(i: &Item) -> impl Iterator<Item = &str> {
    let participants = i
        .live
        .iter()
        .filter_map(|l| l.data["participants"].as_array());
    let types = participants
        .flatten()
        .filter_map(|p| p["document_types"].as_array());
    types.flatten().filter_map(|d| d["label"].as_str())
}

/// `8 checked: 3 valid, 2 invalid, 1 unavailable`, without the zero counts.
pub fn summary(s: &Summary) -> String {
    let counts = [
        (s.valid, "valid"),
        (s.invalid, "invalid"),
        (s.inactive, "inactive"),
        (s.not_found, "not found"),
        (s.unavailable, "unavailable"),
        (s.rate_limited, "rate limited"),
        (s.not_configured, "not configured"),
    ];
    let parts: Vec<String> = counts
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, word)| format!("{n} {word}"))
        .collect();
    format!("{} checked: {}", s.total, parts.join(", "))
}

/// One line per value, its hint and warnings under it, then the summary for 2 or more.
pub fn report(r: &Report) -> String {
    let width = |f: &dyn Fn(&Item) -> usize| {
        let widths = r.results.iter().map(f).filter(|&w| w <= PAD_LIMIT);
        widths.max().unwrap_or(0)
    };
    let vw = width(&|i| shown(i).chars().count());
    let tw = width(&|i| label(i).chars().count());
    let mut out = String::new();
    for i in &r.results {
        let (mark, word, colour) = verdict(i.status);
        let (value, label) = (shown(i), label(i));
        let _ = writeln!(
            out,
            "{colour}{mark}{RESET} {BOLD}{value:<vw$}{RESET}  {DIM}{label:<tw$}{RESET}  {colour}{word}{RESET}{}",
            facts(i)
        );
        let indent = vw + tw + 6;
        let mut documents: Vec<&str> = Vec::new();
        for label in document_labels(i) {
            if !documents.contains(&label) {
                documents.push(label);
                let _ = writeln!(out, "{:indent$}{label}", "");
            }
        }
        let live_hint = i.live.as_ref().and_then(|l| l.hint);
        if let Some(hint) = live_hint.or(i.error.as_ref().and_then(|e| e.hint)) {
            let _ = writeln!(out, "{:indent$}{DIM}{hint}{RESET}", "");
        }
        for w in &i.warnings {
            let _ = writeln!(out, "  {YELLOW}!{RESET} {}", w.message);
        }
    }
    if r.results.len() > 1 {
        let _ = writeln!(out, "{BOLD}{}{RESET}", summary(&r.summary));
    }
    out
}

/// A usage or runtime error's two lines.
pub fn error(e: &Error) -> String {
    let mut out = format!("{RED}✗{RESET} {}\n", e.message);
    if let Some(hint) = e.hint {
        let _ = writeln!(out, "  {DIM}{hint}{RESET}");
    }
    out
}

/// The text with ASCII in place of the symbols a legacy Windows console can't show.
pub fn ascii(text: &str) -> String {
    text.replace(['╭', '╮', '╰', '╯'], "+")
        .replace('─', "-")
        .replace('│', "|")
        .replace('✻', "*")
        .replace('›', ">")
        .replace('✓', "+")
        .replace('✗', "x")
        .replace('·', "-")
        .replace('…', "...")
}

/// Whether a stream shows Unicode: always when it isn't a terminal; on a Windows console only
/// when its output code page is UTF-8.
pub fn unicode(terminal: bool) -> bool {
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn GetConsoleOutputCP() -> u32;
        }
        // SAFETY: takes no arguments and only reads the console's state.
        !terminal || unsafe { GetConsoleOutputCP() } == 65001
    }
    #[cfg(not(windows))]
    {
        let _ = terminal;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_leave_out_zero_counts() {
        let s = Summary {
            total: 8,
            valid: 3,
            invalid: 2,
            inactive: 1,
            unavailable: 1,
            not_configured: 1,
            ..Default::default()
        };
        assert_eq!(
            summary(&s),
            "8 checked: 3 valid, 2 invalid, 1 inactive, 1 unavailable, 1 not configured"
        );
    }

    #[test]
    fn ascii_replaces_every_symbol() {
        assert_eq!(ascii("✓ a · b ✗ c … ! d"), "+ a - b x c ... ! d");
        assert_eq!(ascii("╭─╮│✻ x ›│╰─╯"), "+-+|* x >|+-+");
    }
}
