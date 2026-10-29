//! Runs the built binary the way people and agents do. Nothing here touches the network.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

fn vi(args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_validate-identifier"));
    cmd.args(args)
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .stdin(Stdio::null());
    cmd
}

/// Exit code, stdout and stderr.
fn run(args: &[&str]) -> (i32, String, String) {
    let Output {
        status,
        stdout,
        stderr,
    } = vi(args).output().unwrap();
    let text = |b| String::from_utf8(b).unwrap();
    (status.code().unwrap(), text(stdout), text(stderr))
}

/// The `--json` answer: one line on stdout, nothing on stderr.
fn json(args: &[&str]) -> (i32, Value) {
    json_of(&[args, &["--json"]].concat())
}

fn json_of(args: &[&str]) -> (i32, Value) {
    let (code, out, err) = run(args);
    assert_eq!(err, "", "{args:?}");
    assert_eq!(out.matches('\n').count(), 1, "one line: {out}");
    (code, serde_json::from_str(&out).unwrap())
}

/// A file in the test's scratch folder holding `bytes`.
fn file(name: &str, bytes: &[u8]) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn inputs(v: &Value) -> Vec<&str> {
    let results = v["results"].as_array().unwrap();
    results
        .iter()
        .map(|r| r["input"].as_str().unwrap())
        .collect()
}

#[test]
fn prints_the_version_with_its_data() {
    let want =
        "validate-identifier 0.1.0 (phone data 9.0.42, IBAN registry 103, Peppol code list 9.7)\n";
    for flag in ["--version", "-V"] {
        assert_eq!(run(&[flag]), (0, want.into(), String::new()));
    }
}

#[test]
fn answers_in_the_documented_json_shape() {
    let (code, v) = json(&["check", "de 136 695 976"]);
    assert_eq!(code, 0);
    assert_eq!(
        v,
        json!({
            "results": [{
                "input": "de 136 695 976",
                "type": "vat",
                "value": "DE136695976",
                "country": "DE",
                "status": "valid",
                "checked": "offline",
                "message": "Format and check digits pass. Not checked against VIES; add --live to confirm registration.",
                "offline": {"valid": true, "checks": [
                    {"name": "country", "pass": true},
                    {"name": "format", "pass": true},
                    {"name": "check_digits", "pass": true}
                ]},
                "details": {"prefix": "DE", "number": "136695976", "branch": null},
                "live": null,
                "error": null,
                "warnings": [{"code": "ambiguous_type", "message": "Also a valid eori format; pass --type to check it as one."}]
            }],
            "summary": {"total": 1, "valid": 1, "invalid": 0, "inactive": 0, "not_found": 0,
                        "unavailable": 0, "rate_limited": 0, "not_configured": 0},
            "data": {"version": "0.1.0", "phone_metadata": "9.0.42", "iban_registry": "103",
                     "peppol_code_list": "9.7"}
        })
    );
}

#[test]
fn exits_1_when_any_value_is_invalid() {
    let (code, v) = json(&["check", "DE44500105175407324931", "+44 20 7946 0958"]);
    assert_eq!(code, 0);
    assert_eq!(v["summary"]["valid"], 2);
    let (code, v) = json(&["check", "DE44500105175407324931", "GB82WEST1234569876543"]);
    assert_eq!(code, 1);
    assert_eq!(v["results"][1]["error"]["code"], "iban_length");
    let (code, out, err) = run(&["check", "hello"]);
    assert_eq!((code, err.as_str()), (1, ""));
    assert!(out.contains("hello"), "{out}");
}

#[test]
fn takes_the_type_and_region_options() {
    let (_, v) = json(&["check", "DE136695976", "-t", "eori"]);
    assert_eq!(v["results"][0]["type"], "eori");
    assert_eq!(v["results"][0]["warnings"], json!([]));
    let (code, v) = json(&["check", "07911 123456", "--region", "gb"]);
    assert_eq!(code, 0);
    assert_eq!(v["results"][0]["details"]["region"], "GG");
    let (code, v) = json(&["check", "BE0202239951", "--type", "peppol"]);
    assert_eq!(code, 0);
    assert_eq!(
        v["results"][0]["details"]["candidates"],
        json!(["0208:0202239951", "9925:be0202239951"])
    );
}

#[test]
fn reads_values_from_a_file_or_stdin() {
    let body = "\u{feff}# customers\r\nDE44500105175407324931\r\n\r\n  # not this\r\n  DE136695976  \r\nDE44500105175407324931";
    let path = file("list.txt", body.as_bytes());
    let (code, v) = json(&["check", "-f", path.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert_eq!(
        inputs(&v),
        [
            "DE44500105175407324931",
            "DE136695976",
            "DE44500105175407324931"
        ]
    );
    let mut child = vi(&["check", "-f", "-", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"hello\nDE136695976\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(inputs(&v), ["hello", "DE136695976"]);
}

#[test]
fn holds_the_input_limits() {
    let lines = |n: usize| "DE44500105175407324931\n".repeat(n);
    let ten_thousand = file("10000.txt", lines(10_000).as_bytes());
    let (code, v) = json(&["check", "-f", ten_thousand.to_str().unwrap()]);
    assert_eq!((code, v["summary"]["total"].as_u64()), (0, Some(10_000)));

    let usage = |args: &[&str], message: &str, hint: &str| {
        let (code, out, err) = run(args);
        assert_eq!((code, out.as_str()), (2, ""), "{args:?}");
        assert_eq!(err, format!("✗ {message}\n  {hint}\n"), "{args:?}");
    };
    let too_many = file("10001.txt", lines(10_001).as_bytes());
    let split = "Split the list into smaller files.";
    usage(
        &["check", "-f", too_many.to_str().unwrap()],
        "10001 values; at most 10,000 per run.",
        split,
    );
    let big = file("big.txt", &vec![b'#'; 4 * 1024 * 1024 + 1]);
    let big = big.to_str().unwrap();
    usage(
        &["check", "-f", big],
        &format!("{big} is larger than 4 MB."),
        split,
    );
    let empty = file("empty.txt", b"\n# nothing\n\n");
    usage(
        &["check", "-f", empty.to_str().unwrap()],
        "No values to check.",
        "Pass values, or a file with one value per line.",
    );
    let latin1 = file("latin1.txt", b"M\xfcller\n");
    let latin1 = latin1.to_str().unwrap();
    usage(
        &["check", "-f", latin1],
        &format!("{latin1} isn't UTF-8 text."),
        "Save it as UTF-8, one value per line.",
    );
    usage(
        &["check", "x", "-r", "XX"],
        "Unknown region XX.",
        "Use a 2-letter ISO country code, e.g. GB.",
    );
    usage(
        &[],
        "No command given.",
        "Try: validate-identifier check DE44500105175407324931",
    );

    // A value over 256 bytes is an item error; the other values are still checked.
    let long = "1".repeat(257);
    let (code, v) = json(&["check", &long, "DE44500105175407324931"]);
    assert_eq!(code, 1);
    assert_eq!(v["results"][0]["error"]["code"], "too_long");
    assert_eq!(v["results"][1]["status"], "valid");

    let (code, out, err) = run(&["check", "-f", "no-such-file.txt"]);
    assert_eq!(code, 3);
    assert_eq!(out, "");
    assert_eq!(err, "✗ Couldn't read no-such-file.txt: no such file.\n");

    for args in [
        &["check", "x", "-f", "y"][..],
        &["check"],
        &["check", "x", "-t", "bic"],
        &["nope"],
    ] {
        let (code, out, _) = run(args);
        assert_eq!((code, out.as_str()), (2, ""), "{args:?}");
    }
}

#[test]
fn prints_text_for_people() {
    let (code, out, err) = run(&[
        "check",
        "DE44500105175407324931",
        "+44 20 7946 0958",
        "DE136695976",
        "GB82WEST1234569876543",
        "hello",
    ]);
    assert_eq!((code, err.as_str()), (1, ""));
    assert_eq!(
        out,
        "\
✓ DE44500105175407324931  IBAN · DE   valid · DE44 5001 0517 5407 3249 31
✓ +442079460958           Phone · GB  valid · fixed line · +44 20 7946 0958
✓ DE136695976             VAT · DE    valid · Format and check digits pass. Not checked against VIES; add --live to confirm registration.
  ! Also a valid eori format; pass --type to check it as one.
✗ GB82WEST1234569876543   IBAN · GB   invalid · Wrong length for a GB IBAN (21 characters).
                                      Check for a missing or extra character.
✗ hello                   Unknown     invalid · Can't tell what kind of identifier this is.
                                      Pass --type (iban, vat, phone, email, lei, abn, uid, eori, peppol).
5 checked: 3 valid, 2 invalid
"
    );
    // One value: one line, no summary.
    let (_, out, _) = run(&["check", "DE44500105175407324931"]);
    assert_eq!(
        out,
        "✓ DE44500105175407324931  IBAN · DE  valid · DE44 5001 0517 5407 3249 31\n"
    );
    // A value over 34 characters pushes its own line instead of padding the others.
    let long = "0208:0202239951-and-a-very-long-suffix";
    let (_, out, _) = run(&["check", long, "DE44500105175407324931"]);
    assert!(
        out.contains(&format!("✓ {long}  Peppol     valid · ")),
        "{out}"
    );
    assert!(
        out.contains("✓ DE44500105175407324931  IBAN · DE  valid"),
        "{out}"
    );
}

#[test]
fn colours_only_when_asked_and_never_with_no_color() {
    let args = ["check", "DE44500105175407324931", "hello"];
    let (_, out, _) = run(&args);
    assert!(!out.contains('\x1b'));
    let forced = |no_color: &str| {
        let out = vi(&args)
            .env("CLICOLOR_FORCE", "1")
            .env("NO_COLOR", no_color)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    };
    assert!(!forced("1").contains('\x1b'));
    if cfg!(not(windows)) {
        // Windows turns ANSI codes into console calls, which a pipe can't take.
        let out = forced("");
        assert!(out.starts_with("\x1b[32m✓\x1b[0m \x1b[1mDE44500105175407324931\x1b[0m  \x1b[2mIBAN · DE\x1b[0m  \x1b[32mvalid\x1b[0m"), "{out:?}");
        assert!(out.contains("\x1b[31m✗\x1b[0m"), "{out:?}");
    }
}

#[test]
fn shows_the_sentence_types_and_examples_in_help() {
    for args in [&["--help"][..], &["-h"], &["check", "--help"]] {
        let (code, out, _) = run(args);
        assert_eq!(code, 0);
        assert!(
            out.contains("\nTypes: iban, vat (EU, XI, GB, CH), phone, email, lei, abn, uid (Swiss), eori, peppol.\n"),
            "{args:?}"
        );
        assert!(
            out.contains("\nExamples:\n  validate-identifier check DE44500105175407324931\n"),
            "{args:?}"
        );
        assert!(out.ends_with("NO_COLOR  no colour\n"), "{args:?}");
    }
    let (_, out, _) = run(&["--help"]);
    assert!(
        out.starts_with("Check VAT numbers, IBANs, phone numbers, emails, LEIs and Peppol IDs.\n")
    );
    let (_, out, _) = run(&["check", "--help"]);
    assert!(out.contains("[default: auto]"));
}

/// A list long enough that its answer overflows any pipe buffer.
fn long_list() -> String {
    let path = file(
        "long.txt",
        "DE44500105175407324931\nhello\n".repeat(3000).as_bytes(),
    );
    path.to_str().unwrap().to_string()
}

#[test]
fn ends_quietly_when_stdout_closes() {
    let list = long_list();
    // Each command, and whether it answers on stdout (the others fail on stderr).
    let commands: [(&[&str], bool); 5] = [
        (&["check", "-f", &list], true),
        (&["check", "DE44500105175407324931"], true),
        (&["check", "-r", "XX", "1"], false),
        (&["check", "-f", "no-such-file.txt"], false),
        (&["check", "--frobnicate"], false),
    ];
    for (args, answers) in commands {
        for extra in [&[][..], &["--json"], &["--debug"], &["--json", "--debug"]] {
            let args = [args, extra].concat();
            // Closed before the child starts: closing it after could lose the race to an
            // answer that is already written.
            let (reader, writer) = std::io::pipe().unwrap();
            drop(reader);
            let out = vi(&args)
                .stdout(writer)
                .stderr(Stdio::piped())
                .output()
                .unwrap();
            let err = String::from_utf8(out.stderr).unwrap();
            assert!(!err.contains("panicked"), "{args:?}: {err}");
            if answers {
                let err: String = err.lines().filter(|l| !l.starts_with("debug: ")).collect();
                assert_eq!((out.status.code(), err.as_str()), (Some(0), ""), "{args:?}");
            }
        }
    }
    for args in [&["--help"][..], &["check", "--help"], &["--version"], &[]] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let out = vi(args)
            .stdout(writer)
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert!(
            !String::from_utf8(out.stderr).unwrap().contains("panicked"),
            "{args:?}"
        );
    }
}

#[test]
fn answers_even_when_stderr_closes() {
    let list = long_list();
    for args in [
        &["check", "-f", &list, "--debug"][..],
        &["check", "-f", &list, "--json", "--debug"],
        &["check", "-r", "XX", "1", "--debug"],
        &["check", "hello"],
    ] {
        let want = vi(args).output().unwrap();
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let out = vi(args).stderr(writer).output().unwrap();
        assert_eq!(
            (out.status.code(), out.stdout),
            (want.status.code(), want.stdout),
            "{args:?}"
        );
    }
}

#[test]
fn shows_timings_with_debug() {
    let path = file("two.txt", b"DE44500105175407324931\nhello\n");
    let path = path.to_str().unwrap();
    let (code, out, err) = run(&["check", "-f", path, "--json", "--debug"]);
    assert_eq!(code, 1);
    assert!(serde_json::from_str::<Value>(&out).is_ok());
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(lines.len(), 2, "{err}");
    assert!(
        lines[0].starts_with(&format!("debug: read 2 values from {path} in ")),
        "{err}"
    );
    assert!(
        lines[1].starts_with("debug: checked 2 values offline in "),
        "{err}"
    );
    let (_, plain, _) = run(&["check", "-f", path]);
    let (_, debugged, _) = run(&["check", "-f", path, "--debug"]);
    assert_eq!(plain, debugged);
}

/// The command line that checks one value, which may start with a dash, with `args`.
fn one(value: &str, args: &[&str]) -> Vec<String> {
    let args = [&["check"][..], args, &["--", value]].concat();
    args.into_iter().map(String::from).collect()
}

/// The JSON item for one value checked with `args`.
fn item(value: &str, args: &[&str]) -> (i32, Value) {
    let line = one(value, &[args, &["--json"]].concat());
    let (code, v) = json_of(&line.iter().map(String::as_str).collect::<Vec<_>>());
    (code, v["results"][0].clone())
}

#[test]
fn checks_every_type_offline() {
    let cases: [(&str, &[&str], &str, Value); 9] = [
        (
            "DE44 5001 0517 5407 3249 31",
            &[],
            "iban",
            json!({"country": "DE", "bban": "500105175407324931", "bank_id": "50010517", "branch_id": null, "print": "DE44 5001 0517 5407 3249 31"}),
        ),
        (
            "GR 094 543 092",
            &[],
            "vat",
            json!({"prefix": "EL", "number": "094543092", "branch": null}),
        ),
        (
            "+44 20 7946 0958 ext. 12",
            &[],
            "phone",
            json!({"e164": "+442079460958", "international": "+44 20 7946 0958 x12", "national": "020 7946 0958 x12", "region": "GB", "country_code": 44, "number_type": "fixed_line", "possible": true, "extension": "12"}),
        ),
        (
            "jane@bücher.de",
            &[],
            "email",
            json!({"local": "jane", "domain": "bücher.de", "ascii_domain": "xn--bcher-kva.de", "smtputf8": false}),
        ),
        ("5493001KJTIIGC8Y1R12", &[], "lei", json!({})),
        ("51 824 753 556", &[], "abn", json!({})),
        (
            "CHE-100.000.006",
            &[],
            "uid",
            json!({"display": "CHE-100.000.006"}),
        ),
        ("GB220430231000", &["-t", "eori"], "eori", json!({})),
        (
            "0208:0202239951",
            &[],
            "peppol",
            json!({"candidates": ["0208:0202239951"]}),
        ),
    ];
    for (value, args, kind, details) in cases {
        let (code, i) = item(value, args);
        assert_eq!(code, 0, "{value}: {i}");
        assert_eq!(
            (i["type"].as_str(), i["status"].as_str()),
            (Some(kind), Some("valid")),
            "{value}"
        );
        assert_eq!(i["checked"], "offline");
        assert_eq!(i["details"], details, "{value}");
        assert_eq!((&i["live"], &i["error"]), (&Value::Null, &Value::Null));
        assert_eq!(i["offline"]["valid"], true);
    }
    // The spec's reference values.
    for (value, args, kind) in [
        ("51 824 753 556", &[][..], "abn"),
        ("CHE-116.281.710", &[], "uid"),
        ("CHE-100.000.006", &[], "uid"),
        ("CHE-116.281.710 MWST", &[], "vat"),
        ("5493001KJTIIGC8Y1R12", &[], "lei"),
        ("IE8256796U", &["-t", "eori"], "eori"),
        ("GB220430231000", &["-t", "eori"], "eori"),
        ("BE0202239951", &["-t", "peppol"], "peppol"),
    ] {
        let (code, i) = item(value, args);
        assert_eq!((code, i["type"].as_str()), (0, Some(kind)), "{value}");
    }
    let (_, i) = item("IE8256796U", &["-t", "eori"]);
    assert_eq!(
        i["message"],
        "Format only: EORI numbers have no check digit. Add --live to confirm."
    );
    assert_eq!(i["country"], "IE");
}

#[test]
fn detects_types_and_flags_ambiguous_values() {
    for (value, kind) in [
        ("jane@example.com", "email"),
        ("+44 20 7946 0958", "phone"),
        ("iso6523-actorid-upis::0208:0202239951", "peppol"),
        ("CHE-116.281.710 MWST", "vat"),
        ("CHE-116.281.710", "uid"),
        ("DE44500105175407324931", "iban"),
        ("5493001KJTIIGC8Y1R12", "lei"),
        ("DE136695976", "vat"),
        ("GB2204302310001", "eori"),
        ("51824753556", "abn"),
        ("GB82WEST1234569876543", "iban"),
    ] {
        assert_eq!(item(value, &[]).1["type"], kind, "{value}");
    }
    let (_, i) = item("020 7946 0958", &["-r", "GB"]);
    assert_eq!(i["type"], "phone");
    let (_, i) = item("DE136695976", &[]);
    assert_eq!(
        i["warnings"],
        json!([{"code": "ambiguous_type", "message": "Also a valid eori format; pass --type to check it as one."}])
    );
    let (code, i) = item("DEADBEEF", &[]);
    assert_eq!(code, 1);
    assert_eq!(
        (&i["type"], &i["value"], &i["country"]),
        (&Value::Null, &Value::Null, &Value::Null)
    );
    assert_eq!(i["error"]["code"], "unknown_type");
}

#[test]
fn reaches_every_item_error_with_its_message_and_hint() {
    let long = format!("{}@example.com", "a".repeat(65));
    let label = "a".repeat(63);
    let near_limit = format!("j@{label}.{label}.{label}.{}.com", "b".repeat(57));
    let too_long = "1".repeat(257);
    // value, options, code, message, hint
    type Case<'a> = (&'a str, &'a [&'a str], &'a str, &'a str, Option<&'a str>);
    let cases: Vec<Case> = vec![
        (
            "",
            &[],
            "empty",
            "Empty value.",
            Some("Pass an identifier, e.g. DE136695976."),
        ),
        (
            &too_long,
            &[],
            "too_long",
            "Value is longer than 256 characters.",
            Some("Pass one identifier per value."),
        ),
        (
            "hello",
            &[],
            "unknown_type",
            "Can't tell what kind of identifier this is.",
            Some("Pass --type (iban, vat, phone, email, lei, abn, uid, eori, peppol)."),
        ),
        (
            "XX44500105175407324931",
            &["-t", "iban"],
            "iban_country",
            "XX isn't an IBAN country.",
            Some("An IBAN starts with a country code from the IBAN registry, e.g. DE."),
        ),
        (
            "DE4450010517540732493",
            &[],
            "iban_length",
            "Wrong length for a DE IBAN (21 characters).",
            Some("Check for a missing or extra character."),
        ),
        (
            "GB82WEST123456987654A2",
            &[],
            "iban_format",
            "This doesn't match the account format for GB.",
            Some("Check for letters where digits belong."),
        ),
        (
            "DE44500105175407324932",
            &[],
            "iban_check_digits",
            "The IBAN check digits don't match.",
            Some("Check for a typo or swapped digits."),
        ),
        (
            "XX123456789",
            &["-t", "vat"],
            "vat_country",
            "No VAT rules for XX.",
            Some("Supported: EU countries (EL for Greece), XI, GB and CH."),
        ),
        (
            "123456789",
            &["-t", "vat"],
            "vat_prefix",
            "A VAT number needs its country prefix.",
            Some("Write it as e.g. DE136695976."),
        ),
        (
            "DE12345678",
            &["-t", "vat"],
            "vat_format",
            "DE VAT numbers are DE followed by 9 digits.",
            Some("Check the number on the invoice or registration."),
        ),
        (
            "DE136695977",
            &[],
            "vat_check_digits",
            "The check digits don't match for a DE VAT number.",
            Some("Check for a typo, or add --live to ask VIES."),
        ),
        (
            "07911 123456",
            &["-t", "phone"],
            "phone_region",
            "A phone number without + needs a region.",
            Some("Add --region GB, or write it as +44…."),
        ),
        (
            "+hello",
            &[],
            "phone_parse",
            "This isn't a phone number.",
            Some("Write it in international form, e.g. +44 20 7946 0958."),
        ),
        (
            "+999 123",
            &[],
            "phone_parse",
            "+999 isn't a country calling code.",
            Some("Write it in international form, e.g. +44 20 7946 0958."),
        ),
        (
            "+44 1",
            &[],
            "phone_parse",
            "Too short to be a phone number.",
            Some("Write it in international form, e.g. +44 20 7946 0958."),
        ),
        (
            "+44 123456789012345678901",
            &[],
            "phone_parse",
            "Too long to be a phone number.",
            Some("Write it in international form, e.g. +44 20 7946 0958."),
        ),
        (
            "+44 1234567890123456",
            &[],
            "phone_invalid",
            "+44 1234567890123456 isn't a valid number in GB.",
            Some("Check the number of digits and the area code."),
        ),
        (
            "+44 1632 960000",
            &[],
            "phone_invalid",
            "+44 1632 960000 has the right length for GB but isn't in an assigned range.",
            Some("Check the number of digits and the area code."),
        ),
        (
            "Jane <jane@example.com>",
            &[],
            "email_display_name",
            "Pass only the address, without a name.",
            Some("e.g. jane@example.com"),
        ),
        (
            "jane.example.com",
            &["-t", "email"],
            "email_at",
            "An email address has exactly one @.",
            None,
        ),
        (
            ".jane@example.com",
            &[],
            "email_local",
            "The part before @ isn't valid (dot at the start or end, two dots, or a forbidden character).",
            None,
        ),
        (
            &long,
            &[],
            "email_length",
            "The part before @ is longer than 64 bytes.",
            None,
        ),
        (
            &near_limit,
            &[],
            "email_length",
            "The address is longer than 254 bytes.",
            None,
        ),
        (
            "jane@-example.com",
            &[],
            "email_domain",
            "The domain isn't valid: a part between dots starts or ends with a hyphen.",
            Some("Check the spelling after the @."),
        ),
        (
            "5493001KJTIIGC8Y1R1",
            &["-t", "lei"],
            "lei_format",
            "An LEI is 18 letters or digits followed by 2 check digits.",
            None,
        ),
        (
            "5493001KJTIIGC8Y1R13",
            &[],
            "lei_check_digits",
            "The LEI check digits don't match.",
            Some("Check for a typo, or add --live to ask GLEIF."),
        ),
        (
            "5182475355",
            &["-t", "abn"],
            "abn_format",
            "An ABN is 11 digits.",
            None,
        ),
        (
            "51824753557",
            &["-t", "abn"],
            "abn_check_digits",
            "The ABN check digits don't match.",
            Some("Check for a typo, or add --live to ask ABN Lookup."),
        ),
        (
            "CHE11628171",
            &["-t", "uid"],
            "uid_format",
            "A Swiss UID is CHE followed by 9 digits.",
            Some("e.g. CHE-116.281.710"),
        ),
        (
            "CHE-116.281.711",
            &[],
            "uid_check_digits",
            "The UID check digit doesn't match.",
            None,
        ),
        (
            "CHE-100.000.800",
            &[],
            "uid_check_digits",
            "No UID has this number; its check digit would be 10.",
            None,
        ),
        (
            "GB123",
            &["-t", "eori"],
            "eori_format",
            "A GB EORI is GB followed by 12 to 15 digits.",
            None,
        ),
        (
            "DE1234567890123456",
            &["-t", "eori"],
            "eori_format",
            "An EORI is a country code followed by up to 15 letters or digits.",
            None,
        ),
        (
            "EL123",
            &["-t", "eori"],
            "eori_country",
            "EL isn't an EU country, GB or XI.",
            None,
        ),
        (
            "hello",
            &["-t", "peppol"],
            "peppol_format",
            "A Peppol participant ID looks like 0208:0202239951 (4-digit scheme code, colon, value).",
            Some("Or pass a VAT number, ABN, Swiss UID or LEI with --type peppol."),
        ),
        (
            "XI434031439",
            &["-t", "peppol"],
            "peppol_no_scheme",
            "XI VAT numbers have no Peppol scheme.",
            Some("Pass the participant ID, e.g. 0088:<GLN>."),
        ),
    ];
    for (value, args, code, message, hint) in cases {
        let (exit, i) = item(value, args);
        assert_eq!(exit, 1, "{value}");
        assert_eq!(i["status"], "invalid", "{value}");
        assert_eq!(
            i["error"],
            json!({"code": code, "message": message, "hint": hint}),
            "{value}"
        );
        assert_eq!(i["message"], message);
        assert_eq!(i["offline"]["valid"], false);
        // Text shows the message on the value's line and the hint under it.
        let line = one(value, args);
        let (_, out, _) = run(&line.iter().map(String::as_str).collect::<Vec<_>>());
        assert!(out.contains(&format!("invalid · {message}\n")), "{out}");
        if let Some(hint) = hint {
            assert!(out.contains(&format!("  {hint}\n")), "{out}");
        }
    }
}

#[test]
fn reaches_every_offline_warning() {
    for (value, code, message) in [
        (
            "DE136695976",
            "ambiguous_type",
            "Also a valid eori format; pass --type to check it as one.",
        ),
        (
            "\"jane doe\"@example.com",
            "quoted_local_part",
            "Quoted local parts are valid but many mail systems reject them.",
        ),
        (
            "jane@[192.0.2.1]",
            "address_literal",
            "The domain is an IP address; mail server lookup skipped.",
        ),
    ] {
        let (exit, i) = item(value, &[]);
        assert_eq!((exit, &i["status"]), (0, &json!("valid")), "{value}");
        assert_eq!(i["warnings"], json!([{"code": code, "message": message}]));
        let (_, out, _) = run(&["check", value]);
        assert!(out.ends_with(&format!("\n  ! {message}\n")), "{out}");
    }
}

#[test]
fn turns_away_bad_usage_with_exit_2() {
    for args in [
        &["nope"][..],
        &["check", "x", "--nope"],
        &["check", "x", "-t", "bic"],
        &["check", "x", "-f", "y"],
        &["check"],
        &["check", "x", "-r"],
        &["check", "-f"],
        &["check", "x", "--live"],
    ] {
        let (code, out, err) = run(args);
        assert_eq!((code, out.as_str()), (2, ""), "{args:?}");
        assert!(err.starts_with("error: "), "{args:?}: {err}");
        assert!(
            err.ends_with("For more information, try '--help'.\n"),
            "{args:?}: {err}"
        );
    }
    for region in ["XX", "G", "GBR", "001", "1"] {
        let (code, out, err) = run(&["check", "x", "-r", region]);
        assert_eq!((code, out.as_str()), (2, ""), "{region}");
        assert_eq!(
            err,
            format!("✗ Unknown region {region}.\n  Use a 2-letter ISO country code, e.g. GB.\n")
        );
    }
}
