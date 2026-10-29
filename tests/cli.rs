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
    let args = [args, &["--json"]].concat();
    let (code, out, err) = run(&args);
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
