//! The no-argument mode: each typed or pasted line is a value or a slash command.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rlibphonenumber::enums::Region;

use crate::check::{self, Options};
use crate::item::{Error, Kind};
use crate::phone;
use crate::text::{self, BOLD, DIM, RESET, SENTENCE};

/// What the session commands set.
#[derive(Default)]
pub struct Session {
    pub live: bool,
    pub kind: Option<Kind>,
    pub region: Option<Region>,
    /// Draw the live progress line on stderr.
    pub progress: bool,
}

/// Lines from `input`, grouped: a line and the ones arriving right behind it, as a paste does.
pub fn blocks(input: impl Read + Send + 'static) -> impl Iterator<Item = Vec<String>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut input, mut line) = (BufReader::new(input), Vec::new());
        while input.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
            if tx
                .send(String::from_utf8_lossy(&line).into_owned())
                .is_err()
            {
                break;
            }
            line.clear();
        }
    });
    std::iter::from_fn(move || {
        let mut block = vec![rx.recv().ok()?];
        block.extend(std::iter::from_fn(|| {
            rx.recv_timeout(Duration::from_millis(30)).ok()
        }));
        Some(block)
    })
}

/// Shows the banner, then answers each block of lines until `/quit` or the end of input.
/// `unicode` false swaps the box and marks for ASCII.
pub fn run(
    blocks: impl IntoIterator<Item = Vec<String>>,
    out: &mut impl Write,
    mut session: Session,
    unicode: bool,
) -> io::Result<()> {
    let mut send = |text: String| {
        let text = if unicode { text } else { text::ascii(&text) };
        out.write_all(text.as_bytes())?;
        out.flush()
    };
    send(banner())?;
    let mut blocks = blocks.into_iter();
    loop {
        send(prompt(&session))?;
        let Some(block) = blocks.next() else {
            return send("\n".into());
        };
        let (answer, quit) = reply(&mut session, &block);
        send(answer)?;
        if quit {
            return Ok(());
        }
    }
}

fn banner() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let name = format!("✻ validate-identifier {version}");
    let sentence = format!("  {SENTENCE}");
    let width = name.chars().count().max(sentence.chars().count());
    let row = |plain: &str, styled: String| {
        let pad = " ".repeat(width - plain.chars().count());
        format!("{DIM}│{RESET} {styled}{pad} {DIM}│{RESET}\n")
    };
    let border = "─".repeat(width + 2);
    format!("{DIM}╭{border}╮{RESET}\n")
        + &row(
            &name,
            format!("{BOLD}✻ validate-identifier{RESET} {version}"),
        )
        + &row(&sentence, sentence.clone())
        + &format!("{DIM}╰{border}╯{RESET}\n")
        + &format!("{DIM}  Type or paste values · /live · /type · /region · /help · /quit\n")
        + &format!(
            "  Offline: nothing leaves this computer. /live asks the official registries.{RESET}\n\n"
        )
}

/// `› `, or the settings first: `live peppol GB › `.
fn prompt(s: &Session) -> String {
    let region = s.region.map(|r| r.to_string());
    let settings = [
        s.live.then_some("live"),
        s.kind.map(Kind::name),
        region.as_deref(),
    ];
    let settings: String = settings.iter().flatten().map(|w| format!("{w} ")).collect();
    format!("{BOLD}{settings}›{RESET} ")
}

/// The answers to a block's lines, in order: values between two commands are checked
/// together. True when the block asked to quit.
fn reply(s: &mut Session, block: &[String]) -> (String, bool) {
    let mut out = String::new();
    let mut values = Vec::new();
    for line in block.iter().map(|l| l.trim()) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !line.starts_with('/') {
            values.push(line.to_string());
            continue;
        }
        out += &check(s, &std::mem::take(&mut values));
        if line == "/quit" {
            return (out, true);
        }
        out += &indent(&command(s, line).unwrap_or_else(|e| text::error(&e)));
        out += "\n";
    }
    (out + &check(s, &values), false)
}

fn check(s: &Session, values: &[String]) -> String {
    if values.is_empty() {
        return String::new();
    }
    let o = Options {
        kind: s.kind,
        region: s.region,
        live: s.live,
        progress: s.live && s.progress,
        ..Options::default()
    };
    let answer = check::run(values, &o).map_or_else(|e| text::error(&e), |r| text::report(&r));
    indent(&answer) + "\n"
}

fn command(s: &mut Session, line: &str) -> Result<String, Error> {
    let (command, arg) = line
        .split_once(' ')
        .map_or((line, ""), |(c, a)| (c, a.trim()));
    Ok(match command {
        "/live" => {
            s.live = !s.live;
            if s.live {
                "Live checks on. Each value goes from this computer to its registry (VIES, HMRC, Swiss UID register, ABN Lookup, GLEIF, EOS, Peppol SML); emails send only the domain, to your DNS server. Phone numbers and IBANs stay here.".into()
            } else {
                "Live checks off. Nothing leaves this computer.".into()
            }
        }
        "/type" => {
            s.kind = check::kind(if arg.is_empty() { "auto" } else { arg })?;
            match s.kind {
                Some(k) => format!("Type: {}", text::type_name(Some(k))),
                None => "Type: detected per value".into(),
            }
        }
        "/region" if arg.is_empty() => {
            s.region = None;
            "Phone region: none".into()
        }
        "/region" => {
            let region = phone::region(arg)?;
            s.region = Some(region);
            format!("Phone region: {region} (for numbers without +)")
        }
        "/help" => return Ok(help(s)),
        _ => {
            let message = format!("Unknown command '{command}'.");
            return Err(Error::new("usage", message, Some("Try /help.")));
        }
    } + "\n")
}

fn help(s: &Session) -> String {
    let live = if s.live {
        "on"
    } else {
        "off: nothing leaves this computer"
    };
    let kind = s.kind.map_or("auto", Kind::name);
    let region = s.region.map_or("none".into(), |r| r.to_string());
    format!(
        "Type or paste identifiers, one per line:
  DE44500105175407324931    IBAN
  DE136695976               VAT number (EU with EL for Greece, XI, GB, CHE-… MWST)
  +44 20 7946 0958          phone number (or 020 7946 0958 after /region GB)
  jane@example.com          email address
  5493001KJTIIGC8Y1R12      LEI
  51 824 753 556            Australian ABN
  CHE-116.281.710           Swiss UID
  0208:0202239951           Peppol participant (or a VAT number after /type peppol)
/live        ask the official registries (now {live})
/type T      auto, iban, vat, phone, email, lei, abn, uid, eori, peppol (now {kind})
/region CC   region for phone numbers without + (now {region})
/quit        leave (Ctrl+C works too)
"
    )
}

fn indent(text: &str) -> String {
    text.lines().map(|l| format!("  {l}\n")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The session's output without colour, each inner list arriving as one block.
    fn session(blocks: &[&[&str]]) -> String {
        let blocks = blocks
            .iter()
            .map(|b| b.iter().map(|l| format!("{l}\n")).collect());
        let mut out = vec![];
        run(blocks, &mut out, Session::default(), true).unwrap();
        anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string()
    }

    #[test]
    fn shows_the_banner_and_checks_values() {
        let out = session(&[&["DE44500105175407324931"], &["/quit"]]);
        assert_eq!(
            out,
            "╭─────────────────────────────────────────────────────────────────────────╮
│ ✻ validate-identifier 0.1.0                                             │
│   Check VAT numbers, IBANs, phone numbers, emails, LEIs and Peppol IDs. │
╰─────────────────────────────────────────────────────────────────────────╯
  Type or paste values · /live · /type · /region · /help · /quit
  Offline: nothing leaves this computer. /live asks the official registries.

›   ✓ DE44500105175407324931  IBAN · DE  valid · DE44 5001 0517 5407 3249 31

› "
        );
    }

    #[test]
    fn a_pasted_block_is_checked_together_with_a_summary() {
        let out = session(&[&["DE44500105175407324931", "", "# a note", "hello"]]);
        assert!(out.ends_with(
            "›   ✓ DE44500105175407324931  IBAN · DE  valid · DE44 5001 0517 5407 3249 31
  ✗ hello                   Unknown    invalid · Can't tell what kind of identifier this is.
                                       Pass --type (iban, vat, phone, email, lei, abn, uid, eori, peppol).
  2 checked: 1 valid, 1 invalid

› \n"
        ));
    }

    #[test]
    fn keeps_session_settings() {
        let out = session(&[
            &["/live"],
            &["/type vat"],
            &["/region gb"],
            &["/help"],
            &["/type auto", "07911 123456", "/region", "/live"],
            &["/type"],
            &["/help"],
        ]);
        for want in [
            "›   Live checks on. Each value goes from this computer to its registry (VIES, HMRC, Swiss UID register, ABN Lookup, GLEIF, EOS, Peppol SML); emails send only the domain, to your DNS server. Phone numbers and IBANs stay here.\n\nlive › ",
            "live ›   Type: VAT\n\nlive vat › ",
            "live vat ›   Phone region: GB (for numbers without +)\n\nlive vat GB › ",
            "  /live        ask the official registries (now on)\n",
            "  /type T      auto, iban, vat, phone, email, lei, abn, uid, eori, peppol (now vat)\n",
            "  /region CC   region for phone numbers without + (now GB)\n",
            "live vat GB ›   Type: detected per value\n\n  ✓ +447911123456  Phone · GG  valid · mobile · +44 7911 123456\n\n  Phone region: none\n\n  Live checks off. Nothing leaves this computer.\n\n› ",
            "›   Type: detected per value\n\n› ",
            "  /live        ask the official registries (now off: nothing leaves this computer)\n",
        ] {
            assert!(out.contains(want), "missing {want:?} in\n{out}");
        }
    }

    #[test]
    fn explains_bad_commands_and_quits_mid_block() {
        let out = session(&[
            &["/type x"],
            &["/region XX"],
            &["/frob"],
            &["DE136695976", "/quit", "hello"],
        ]);
        for want in [
            "›   ✗ Unknown type 'x'.\n    Use one of: auto, iban, vat, phone, email, lei, abn, uid, eori, peppol.\n\n› ",
            "›   ✗ Unknown region XX.\n    Use a 2-letter ISO country code, e.g. GB.\n\n› ",
            "›   ✗ Unknown command '/frob'.\n    Try /help.\n\n› ",
        ] {
            assert!(out.contains(want), "missing {want:?} in\n{out}");
        }
        assert!(
            out.ends_with("pass --type to check it as one.\n\n"),
            "{out}"
        );
        assert!(!out.contains("hello"), "{out}");
        let mut ascii = vec![];
        run([vec!["x".into()]], &mut ascii, Session::default(), false).unwrap();
        let ascii = anstream::adapter::strip_str(&String::from_utf8(ascii).unwrap()).to_string();
        assert!(ascii.starts_with("+-----") && ascii.contains("| * validate-identifier"));
        assert!(ascii.contains("\n>   x x  "), "{ascii}");
    }

    #[test]
    fn blocks_keep_every_line_in_order() {
        let lines: Vec<String> = blocks(&b"a\nb\r\n\xffc"[..]).flatten().collect();
        assert_eq!(lines, ["a\n", "b\r\n", "\u{fffd}c"]);
    }
}
