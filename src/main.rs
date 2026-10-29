use std::fs::File;
use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::LazyLock;

use clap::{Parser, Subcommand};
use validate_identifier::check::{self, Options, usage};
use validate_identifier::item::{Error, Kind};
use validate_identifier::phone;

static VERSION: LazyLock<String> = LazyLock::new(validate_identifier::version);

const SENTENCE: &str = "Check VAT numbers, IBANs, phone numbers, emails, LEIs and Peppol IDs.";

const EXAMPLES: &str =
    "Types: iban, vat (EU, XI, GB, CH), phone, email, lei, abn, uid (Swiss), eori, peppol.

\x1b[1;4mExamples:\x1b[0m
  validate-identifier check DE44500105175407324931
  validate-identifier check \"+44 20 7946 0958\" \"07911 123456\" -r GB
  validate-identifier check BE0202239951 -t peppol
  validate-identifier check -f customers.txt --json";

const TYPES: [&str; 10] = [
    "auto", "iban", "vat", "phone", "email", "lei", "abn", "uid", "eori", "peppol",
];

#[derive(Parser)]
#[command(
    name = "validate-identifier",
    bin_name = "validate-identifier",
    about = SENTENCE,
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
    /// Check identifiers
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
        /// Region for phone numbers without +
        #[arg(short, long, value_name = "CC")]
        region: Option<String>,
        /// Read values from a file, - for stdin
        #[arg(short, long, value_name = "FILE")]
        file: Option<PathBuf>,
        /// Print one JSON object
        #[arg(long)]
        json: bool,
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

/// The report as JSON or one line per value, and its exit code.
fn run(command: Command) -> Result<(String, u8), Error> {
    let Command::Check {
        values,
        kind,
        region,
        file,
        json,
    } = command;
    let values = match file {
        Some(path) => read_values(&path)?,
        None => values,
    };
    let o = Options {
        kind: Kind::ALL.into_iter().find(|k| k.name() == kind),
        region: region.as_deref().map(phone::region).transpose()?,
    };
    let report = check::run(&values, &o)?;
    let out = if json {
        serde_json::to_string(&report).expect("results are plain data") + "\n"
    } else {
        let line = |i: &validate_identifier::item::Item| {
            format!(
                "{}  {:?}  {}\n",
                i.value.as_ref().unwrap_or(&i.input),
                i.status,
                i.message
            )
        };
        report.results.iter().map(line).collect()
    };
    Ok((out, report.summary.exit_code()))
}

fn main() -> ExitCode {
    let answer = if std::env::args_os().len() == 1 {
        let hint = "Try: validate-identifier check DE44500105175407324931";
        Err(usage("No command given.", hint))
    } else {
        run(Cli::parse().command)
    };
    match answer {
        Ok((out, code)) => {
            print!("{out}");
            ExitCode::from(code)
        }
        Err(e) => {
            eprintln!("✗ {}", e.message);
            if let Some(hint) = e.hint {
                eprintln!("  {hint}");
            }
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
