//! Talks to `validate-identifier mcp` over stdio the way MCP clients do.

mod common;

use std::io::Write;
use std::process::Stdio;

use common::{Mock, command, ended, reply, stderr_of};
use serde_json::{Value, json};

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#;

/// Initializes, sends each message, closes stdin, and returns the responses by id and the
/// exit code.
fn session(messages: &[Value], env: &[(&str, &str)]) -> (Vec<Value>, i32) {
    let mut child = command(&["mcp"], env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "{INITIALIZE}").unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
    )
    .unwrap();
    for m in messages {
        writeln!(stdin, "{m}").unwrap();
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    let err = String::from_utf8(out.stderr).unwrap();
    assert_eq!(err, "");
    let mut responses: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    responses.sort_by_key(|r| r["id"].as_u64());
    (responses, out.status.code().unwrap())
}

fn call(id: u64, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": "validate_identifiers", "arguments": arguments}})
}

/// The CLI's `--json` answer for the same input.
fn cli(args: &[&str]) -> Value {
    let out = command(&[&["check", "--json"], args].concat(), &[])
        .output()
        .unwrap();
    serde_json::from_slice(&out.stdout).unwrap()
}

const DESCRIPTION: &str = "Check identifiers and say whether they are valid: IBAN (registry format and check digits); VAT numbers for the EU, Northern Ireland (XI), the UK and Switzerland (format and check digits, or live from VIES, HMRC or the Swiss UID register, with the registered name); phone numbers (valid, mobile or fixed line, region, E.164 and national formats); email addresses (syntax, and mail servers when live); LEI (GLEIF registration status); Australian ABN (ABN Lookup); Swiss UID; EORI (EU EOS, HMRC); Peppol participants (is it registered on Peppol, which document types it accepts; also accepts a VAT number, ABN, UID or LEI). The type is detected when omitted. Offline by default; live true asks the registries over the network. A registry outage gives status unavailable or rate_limited, never invalid, and the offline result is still returned. UK VAT and ABN live checks need the user's own credentials in environment variables; without them the status is not_configured. On bad input the result has isError true and one text block holding {\"error\":{\"code\",\"message\",\"hint\"}}, hint null when there is none.";

#[test]
fn lists_one_tool_with_its_schemas() {
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let (responses, code) = session(&[list], &[]);
    assert_eq!(code, 0);
    let init = &responses[0]["result"];
    assert_eq!(
        init["serverInfo"],
        json!({"name": "validate-identifier", "version": "0.1.0"})
    );
    assert_eq!(
        init["instructions"],
        "Validate IBANs, VAT numbers, phone numbers, email addresses, LEIs, Australian ABNs, Swiss UIDs, EORI numbers and Peppol participant IDs. Offline checks are instant; set live to true to confirm with the official registry (VIES, HMRC, the Swiss UID register, ABN Lookup, GLEIF, EOS, the Peppol SML, DNS). Use this instead of writing check-digit code or calling registries yourself."
    );
    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    let t = &tools[0];
    assert_eq!(t["name"], "validate_identifiers");
    assert_eq!(t["description"], DESCRIPTION);
    assert_eq!(
        t["annotations"],
        json!({"readOnlyHint": true, "idempotentHint": true, "openWorldHint": true})
    );
    // Rust's number formats (uint, uint64) aren't JSON Schema's, and clients warn about them.
    assert!(!t.to_string().contains("\"format\":"), "{t}");
    let input = &t["inputSchema"];
    assert_eq!(input["type"], "object");
    assert_eq!(input["additionalProperties"], false);
    assert_eq!(input["required"], json!(["values"]));
    let p = &input["properties"];
    let mut names: Vec<&String> = p.as_object().unwrap().keys().collect();
    names.sort();
    assert_eq!(names, ["live", "region", "requester_vat", "type", "values"]);
    assert_eq!(
        (&p["values"]["minItems"], &p["values"]["maxItems"]),
        (&json!(1), &json!(1000))
    );
    assert_eq!(
        p["values"]["items"],
        json!({"type": "string", "minLength": 1, "maxLength": 256})
    );
    assert_eq!(
        p["type"]["enum"],
        json!([
            "auto", "iban", "vat", "phone", "email", "lei", "abn", "uid", "eori", "peppol"
        ])
    );
    assert_eq!(
        (&p["type"]["default"], &p["live"]["default"]),
        (&json!("auto"), &json!(false))
    );
    assert_eq!(
        (&p["region"]["minLength"], &p["region"]["maxLength"]),
        (&json!(2), &json!(2))
    );
    let output = &t["outputSchema"];
    assert_eq!(output["type"], "object");
    assert_eq!(output["required"], json!(["results", "summary", "data"]));
    let item = &output["$defs"]["Item"]["required"];
    for field in [
        "input", "type", "status", "checked", "offline", "live", "error", "warnings",
    ] {
        assert!(item.as_array().unwrap().contains(&json!(field)), "{field}");
    }
}

/// A successful call: structured content and its text both the CLI's JSON.
fn check_answer(response: &Value, cli_args: &[&str]) {
    let result = &response["result"];
    assert_eq!(result["isError"], false, "{cli_args:?}: {result}");
    let want = cli(cli_args);
    assert_eq!(result["structuredContent"], want, "{cli_args:?}");
    let text = result["content"][0]["text"].as_str().unwrap();
    assert_eq!(serde_json::from_str::<Value>(text).unwrap(), want);
}

#[test]
fn answers_like_the_cli() {
    let calls = [
        (
            json!({"values": ["DE136695976", "+44 20 7946 0958", "GB82WEST1234569876543", "hello"]}),
            &[
                "DE136695976",
                "+44 20 7946 0958",
                "GB82WEST1234569876543",
                "hello",
            ][..],
        ),
        (
            json!({"values": ["DE136695976", "DE136695976"], "type": "eori", "live": false}),
            &["DE136695976", "DE136695976", "-t", "eori"],
        ),
        (
            json!({"values": ["07911 123456"], "type": "auto", "region": "gb"}),
            &["07911 123456", "-r", "gb"],
        ),
        (
            json!({"values": ["0208:0202239951", "x".repeat(257)], "requester_vat": "DE136695976"}),
            &[
                "0208:0202239951",
                &"x".repeat(257),
                "--requester",
                "DE136695976",
            ],
        ),
    ];
    let messages: Vec<Value> = (0..calls.len())
        .map(|i| call(i as u64 + 2, calls[i].0.clone()))
        .collect();
    let (responses, code) = session(&messages, &[]);
    assert_eq!(code, 0);
    for (i, (_, args)) in calls.iter().enumerate() {
        check_answer(&responses[i + 1], args);
    }
}

#[test]
fn refuses_bad_arguments_with_the_error_object() {
    let many = |n: usize| json!(vec!["DE136695976"; n]);
    let calls = [
        (
            json!({"values": ["x"], "live": true, "foo": 1}),
            "bad_request",
            "Can't read the arguments: unknown field `foo`, expected one of `values`, `type`, `live`, `region`, `requester_vat`.",
            None,
        ),
        (json!({"values": "DE136695976"}), "bad_request", "", None),
        (json!({}), "bad_request", "", None),
        (
            json!({"values": ["x"], "live": "yes"}),
            "bad_request",
            "",
            None,
        ),
        (
            json!({"values": []}),
            "usage",
            "No values to check.",
            Some("Pass at least one value."),
        ),
        (
            json!({"values": many(1001)}),
            "usage",
            "1001 values; at most 1,000 per call.",
            Some("Split the list."),
        ),
        (
            json!({"values": many(101), "live": true}),
            "usage",
            "101 values; at most 100 per call with live true.",
            Some("Split the list, or set live to false."),
        ),
        (
            json!({"values": ["x"], "type": "isbn"}),
            "usage",
            "Unknown type 'isbn'.",
            Some("Use one of: auto, iban, vat, phone, email, lei, abn, uid, eori, peppol."),
        ),
        (
            json!({"values": ["x"], "region": "XX"}),
            "usage",
            "Unknown region XX.",
            Some("Use a 2-letter ISO country code, e.g. GB."),
        ),
        (
            json!({"values": ["x"], "requester_vat": "DE1"}),
            "usage",
            "--requester DE1 isn't a valid VAT number.",
            Some("Pass your own VAT number with its country prefix, e.g. DE136695976."),
        ),
    ];
    let messages: Vec<Value> = (0..calls.len())
        .map(|i| call(i as u64 + 2, calls[i].0.clone()))
        .collect();
    let (responses, code) = session(&messages, &[]);
    assert_eq!(code, 0);
    for (i, (args, code, message, hint)) in calls.iter().enumerate() {
        let result = &responses[i + 1]["result"];
        assert_eq!(result["isError"], true, "{args}: {result}");
        assert!(result.get("structuredContent").is_none(), "{args}");
        let content = result["content"].as_array().unwrap();
        assert_eq!(content.len(), 1, "{args}");
        let text = content[0]["text"].as_str().unwrap();
        let error: Value = serde_json::from_str(text).unwrap();
        let keys: Vec<&String> = error["error"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["code", "hint", "message"], "{args}");
        assert!(text.starts_with(r#"{"error":{"code":"#), "{text}");
        assert_eq!(error["error"]["code"], *code, "{args}: {text}");
        if !message.is_empty() {
            assert_eq!(error["error"]["message"], *message, "{args}");
            assert_eq!(error["error"]["hint"], json!(hint), "{args}");
        } else {
            let message = error["error"]["message"].as_str().unwrap();
            assert!(
                message.starts_with("Can't read the arguments: "),
                "{message}"
            );
        }
    }
}

#[test]
fn checks_live_through_the_registry_overrides() {
    let vies = json!({
        "countryCode": "DE", "vatNumber": "136695976", "requestDate": "2026-10-29T10:12:03.524Z",
        "valid": true, "requestIdentifier": "", "name": "EXAMPLE GMBH", "address": "1 TEST STREET"
    });
    let mock = Mock::start(move |_| reply(200, vies.to_string()));
    let arguments =
        json!({"values": ["DE136695976", "GB123456782", "DE44500105175407324931"], "live": true});
    let (responses, code) = session(&[call(2, arguments)], &mock.env());
    assert_eq!(code, 0);
    let result = &responses[1]["result"];
    assert_eq!(result["isError"], false);
    let results = &result["structuredContent"]["results"];
    assert_eq!(
        (&results[0]["status"], &results[0]["checked"]),
        (&json!("valid"), &json!("live"))
    );
    assert_eq!(results[0]["live"]["data"]["name"], "EXAMPLE GMBH");
    // UK VAT needs the user's own HMRC credentials, and an IBAN never leaves the computer.
    assert_eq!(results[1]["status"], "not_configured");
    assert_eq!(results[2]["live"], Value::Null);
    let sent: Vec<String> = mock.requests().iter().map(|r| r.target.clone()).collect();
    assert_eq!(sent, ["/taxation_customs/vies/rest-api/check-vat-number"]);
}

#[test]
fn ends_when_stdin_closes() {
    let (responses, code) = session(&[], &[]);
    assert_eq!((responses.len(), code), (1, 0));
    let out = command(&["mcp"], &[]).output().unwrap();
    assert_eq!(
        (out.status.code(), out.stdout.len(), out.stderr.len()),
        (Some(0), 0, 0)
    );
}

#[test]
fn ends_quietly_when_stdout_closes() {
    // The read end closes before the spawn: dropped after it, a test spawning beside this
    // one could still hold a copy when the server writes.
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let mut child = command(&["mcp"], &[])
        .stdin(Stdio::piped())
        .stdout(writer)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let _ = writeln!(stdin, "{INITIALIZE}");
    let code = ended(&mut child);
    drop(stdin);
    assert_eq!((code, stderr_of(&mut child)), (Some(0), String::new()));
}

#[test]
fn ends_quietly_when_the_client_leaves_mid_session() {
    let values = vec!["DE44500105175407324931"; 1000];
    let big = call(2, json!({ "values": values })).to_string();
    for half_line in [true, false] {
        let mut child = command(&["mcp"], &[])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "{INITIALIZE}").unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
        )
        .unwrap();
        if half_line {
            // The client dies halfway through a message.
            write!(stdin, "{}", &big[..40]).unwrap();
        } else {
            // The client stops reading while a large answer is on its way, then leaves.
            writeln!(stdin, "{big}").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(child.stdout.take());
        }
        drop(stdin);
        let code = ended(&mut child);
        let err = stderr_of(&mut child);
        assert!(!err.contains("panicked"), "{err}");
        assert_eq!(code, Some(0), "{err}");
    }
}

#[test]
#[ignore = "known bug: F08"]
fn refuses_values_outside_the_advertised_lengths() {
    let long = "A".repeat(257);
    let calls = [
        call(2, json!({"values": [""]})),
        call(3, json!({"values": [long]})),
    ];
    let (responses, code) = session(&calls, &[]);
    assert_eq!(code, 0);
    for r in &responses[1..] {
        let result = &r["result"];
        assert_eq!(result["isError"], true, "{r}");
        assert!(result.get("structuredContent").is_none(), "{r}");
        let text = result["content"][0]["text"].as_str().unwrap();
        let error: Value = serde_json::from_str(text).unwrap();
        assert!(error["error"]["message"].is_string(), "{r}");
    }
}
