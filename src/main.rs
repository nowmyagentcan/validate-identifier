use std::fs::File;
use std::io::{self, ErrorKind, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::LazyLock;
use std::time::Instant;

use clap::{Parser, Subcommand};
use validate_identifier::check::{self, Options, usage};
use validate_identifier::interactive::{self, Session};
use validate_identifier::item::Error;
use validate_identifier::live;
use validate_identifier::{phone, text};

static VERSION: LazyLock<String> = LazyLock::new(validate_identifier::version);

const EXAMPLES: &str =
    "Types: iban, vat (EU, XI, GB, CH), phone, email, lei, abn, uid (Swiss), eori, peppol.
Nothing leaves this computer unless you pass --live.
UK VAT and ABN checks need your own credentials (below); none come with this tool.

\x1b[1;4mEnvironment (live checks):\x1b[0m
  HMRC_CLIENT_ID, HMRC_CLIENT_SECRET   UK VAT checks with your own HMRC application
  ABN_LOOKUP_GUID                      ABN checks with your own ABN Lookup GUID
  VALIDATE_IDENTIFIER_HMRC_URL         HMRC base URL (sandbox: https://test-api.service.hmrc.gov.uk)
  VALIDATE_IDENTIFIER_VIES_URL, _EOS_URL, _UID_URL, _ABN_URL, _GLEIF_URL
                                       other base URLs (tests, mirrors)
  VALIDATE_IDENTIFIER_PEPPOL_SML       Peppol SML zone
  HTTPS_PROXY, NO_PROXY                proxy for HTTPS requests
  NO_COLOR                             no colour

\x1b[1;4mExamples:\x1b[0m
  validate-identifier check DE44500105175407324931
  validate-identifier check \"+44 20 7946 0958\" \"07911 123456\" -r GB
  validate-identifier check DE136695976 --live
  validate-identifier check CHE-116.281.710 --live
  validate-identifier check 0208:0202239951 --live
  validate-identifier check BE0202239951 -t peppol --live
  validate-identifier check -f customers.txt --live --json";

const TYPES: [&str; 10] = [
    "auto", "iban", "vat", "phone", "email", "lei", "abn", "uid", "eori", "peppol",
];

#[derive(Parser)]
#[command(
    name = "validate-identifier",
    bin_name = "validate-identifier",
    about = text::SENTENCE,
    override_usage = "validate-identifier [COMMAND]",
    version = VERSION.as_str(),
    after_help = EXAMPLES,
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check identifiers, offline or with the official registries
    #[command(
        after_help = EXAMPLES,
        override_usage = "validate-identifier check <VALUE>... [OPTIONS]
       validate-identifier check -f <FILE> [OPTIONS]"
    )]
    Check {
        /// Values to check
        #[arg(
            value_name = "VALUE",
            required_unless_present = "file",
            conflicts_with = "file"
        )]
        values: Vec<String>,
        /// Identifier type: auto, iban, vat, phone, email, lei, abn, uid, eori, peppol
        #[arg(short = 't', long = "type", value_name = "T", default_value = "auto",
              value_parser = TYPES, hide_possible_values = true)]
        kind: String,
        /// Ask the official registries over the network
        #[arg(short, long)]
        live: bool,
        /// Your own VAT number, for a VIES or HMRC consultation number
        #[arg(long, value_name = "VAT")]
        requester: Option<String>,
        /// DNS server for email and Peppol lookups
        #[arg(long, value_name = "IP[:PORT]")]
        dns: Option<String>,
        /// Region for phone numbers without +
        #[arg(short, long, value_name = "CC")]
        region: Option<String>,
        /// Read values from a file, - for stdin
        #[arg(short, long, value_name = "FILE")]
        file: Option<PathBuf>,
        /// Print one JSON object
        #[arg(long)]
        json: bool,
        /// Show requests and timings on stderr (secrets hidden)
        #[arg(long)]
        debug: bool,
    },
}

/// A file is read whole up to this size.
const MAX_FILE: u64 = 4 * 1024 * 1024;

/// The values in a file or stdin: one per line, without blank lines and `#` lines.
fn read_values(path: &Path) -> Result<Vec<String>, Error> {
    let name = path.display();
    let mut bytes = Vec::new();
    let read = if path == Path::new("-") {
        io::stdin()
            .lock()
            .take(MAX_FILE + 1)
            .read_to_end(&mut bytes)
    } else {
        File::open(path).and_then(|f| f.take(MAX_FILE + 1).read_to_end(&mut bytes))
    };
    read.map_err(|e| {
        let why = match e.kind() {
            ErrorKind::NotFound => "no such file".to_string(),
            kind => kind.to_string(),
        };
        Error::new("io", format!("Couldn't read {name}: {why}."), None)
    })?;
    if bytes.len() as u64 > MAX_FILE {
        let message = format!("{name} is larger than 4 MB.");
        return Err(usage(message, "Split the list into smaller files."));
    }
    let text = String::from_utf8(bytes).map_err(|_| {
        usage(
            format!("{name} isn't UTF-8 text."),
            "Save it as UTF-8, one value per line.",
        )
    })?;
    let lines = text.strip_prefix('\u{feff}').unwrap_or(&text).lines();
    Ok(lines
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect())
}

/// The report as JSON or text, and its exit code.
fn run(command: Command) -> Result<(String, u8), Error> {
    let Command::Check {
        values,
        kind,
        live,
        requester,
        dns,
        region,
        file,
        json,
        debug,
    } = command;
    live::set_debug(debug);
    let start = Instant::now();
    let values = match file {
        Some(path) => {
            let values = read_values(&path)?;
            let n = values.len();
            live::debug(format!(
                "read {n} values from {} in {:.1?}",
                path.display(),
                start.elapsed()
            ));
            values
        }
        None => values,
    };
    let o = Options {
        kind: check::kind(&kind)?,
        region: region.as_deref().map(phone::region).transpose()?,
        live,
        progress: live && !json && io::stderr().is_terminal(),
        requester,
        dns: dns
            .as_deref()
            .map(validate_identifier::source::dns::server)
            .transpose()?,
    };
    let start = Instant::now();
    let report = check::run(&values, &o)?;
    let n = report.results.len();
    live::debug(format!("checked {n} values in {:.1?}", start.elapsed()));
    let out = if json {
        serde_json::to_string(&report).expect("results are plain data") + "\n"
    } else {
        people(text::report(&report), &io::stdout())
    };
    Ok((out, report.summary.exit_code()))
}

/// Text for a person, in ASCII when their console can't show Unicode.
fn people(text: String, stream: &impl IsTerminal) -> String {
    if text::unicode(stream.is_terminal()) {
        text
    } else {
        text::ascii(&text)
    }
}

/// Writes the answer to stdout; a closed stdout ends quietly.
fn emit(out: &str, code: u8) -> ExitCode {
    let mut stdout = anstream::stdout().lock();
    match stdout
        .write_all(out.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Ok(()) => ExitCode::from(code),
        Err(e) if e.kind() == ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            fail(&Error::new(
                "io",
                format!("Couldn't write output: {e}."),
                None,
            ));
            ExitCode::from(3)
        }
    }
}

/// An error's lines on stderr; a closed stderr loses them quietly.
fn fail(e: &Error) {
    let _ = write!(
        anstream::stderr(),
        "{}",
        people(text::error(e), &io::stderr())
    );
}

/// The banner and a prompt on a terminal; with no terminal to ask on, a usage error.
fn interactive() -> ExitCode {
    if !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
        let hint = "Try: validate-identifier check DE44500105175407324931";
        fail(&usage("No command given.", hint));
        return ExitCode::from(2);
    }
    let session = Session {
        progress: io::stderr().is_terminal(),
        ..Session::default()
    };
    let blocks = interactive::blocks(io::stdin());
    match interactive::run(
        blocks,
        &mut anstream::stdout(),
        session,
        text::unicode(true),
    ) {
        Err(e) if e.kind() != ErrorKind::BrokenPipe => {
            fail(&Error::new(
                "io",
                format!("Couldn't write output: {e}."),
                None,
            ));
            ExitCode::from(3)
        }
        _ => ExitCode::SUCCESS,
    }
}

fn main() -> ExitCode {
    if std::env::args_os().len() == 1 {
        return interactive();
    }
    match run(Cli::parse().command) {
        Ok((out, code)) => emit(&out, code),
        Err(e) => {
            fail(&e);
            ExitCode::from(if e.code == "io" { 3 } else { 2 })
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn checks_the_command_line() {
        use clap::CommandFactory;
        super::Cli::command().debug_assert();
    }
}
