//! Live checks through the binary against the loopback mock: every documented answer, the
//! exact requests, and that a failed request is never a verdict.

mod common;

use common::{Answer, Mock, closed_port, reply, vi};
use serde_json::{Value, json};

const UA: &str =
    "validate-identifier/0.1.0 (+https://github.com/nowmyagentcan/validate-identifier)";

fn vies_ok(name: &str) -> String {
    json!({
        "countryCode": "DE", "vatNumber": "136695976", "requestDate": "2026-10-29T10:12:03.524Z",
        "valid": true, "requestIdentifier": "", "name": name, "address": "1 TEST STREET",
        "traderName": "---", "traderNameMatch": "NOT_PROCESSED"
    })
    .to_string()
}

fn vies_error(code: &str) -> Answer {
    reply(
        200,
        json!({"actionSucceed": false, "errorWrappers": [{"error": code}]}).to_string(),
    )
}

fn first(v: &Value) -> &Value {
    &v["results"][0]
}

#[test]
fn asks_vies_with_the_documented_request_and_reads_its_answer() {
    let mock = Mock::start(|_| reply(200, vies_ok("EXAMPLE GMBH")));
    let (code, v) = mock.json(&["check", "de 136 695 976", "--live"], &[]);
    assert_eq!(code, 0);
    let r = first(&v);
    assert_eq!(
        (r["status"].as_str(), r["checked"].as_str()),
        (Some("valid"), Some("live"))
    );
    assert_eq!(
        r["message"],
        "Registered for VAT (VIES, 2026-10-29 10:12 UTC)."
    );
    assert_eq!(r["offline"]["valid"], true);
    assert_eq!(
        r["live"],
        json!({
            "source": "VIES", "status": "active", "checked_at": "2026-10-29T10:12:03Z",
            "code": null, "retry_after": null,
            "data": {"name": "EXAMPLE GMBH", "address": "1 TEST STREET", "consultation_number": null}
        })
    );
    let req = &mock.requests()[0];
    assert_eq!(req.method, "POST");
    assert_eq!(
        req.target,
        "/taxation_customs/vies/rest-api/check-vat-number"
    );
    assert_eq!(req.header("user-agent"), Some(UA));
    assert_eq!(req.header("content-type"), Some("application/json"));
    assert_eq!(
        req.json(),
        json!({"countryCode": "DE", "vatNumber": "136695976"})
    );
}

#[test]
fn sends_greek_and_northern_irish_numbers_as_written() {
    let mock = Mock::start(|_| reply(200, vies_ok("---")));
    let (code, v) = mock.json(&["check", "GR094543092", "XI434031439", "--live"], &[]);
    assert_eq!(code, 0);
    let sent: Vec<Value> = mock.requests().iter().map(|r| r.json()).collect();
    assert_eq!(
        sent,
        [
            json!({"countryCode": "EL", "vatNumber": "094543092"}),
            json!({"countryCode": "XI", "vatNumber": "434031439"})
        ]
    );
    // VIES writes "---" for a name it doesn't share.
    assert_eq!(first(&v)["live"]["data"]["name"], Value::Null);
}

#[test]
fn maps_every_vies_answer_and_never_calls_a_failure_invalid() {
    let cases = [
        (
            r#"{"valid": false, "requestDate": "2026-10-29T10:12:03Z"}"#,
            "not_found",
            1,
            None,
        ),
        ("INVALID_INPUT", "not_found", 1, Some("INVALID_INPUT")),
        ("MS_UNAVAILABLE", "unavailable", 3, Some("MS_UNAVAILABLE")),
        (
            "SERVICE_UNAVAILABLE",
            "unavailable",
            3,
            Some("SERVICE_UNAVAILABLE"),
        ),
        ("TIMEOUT", "unavailable", 3, Some("TIMEOUT")),
        ("VAT_BLOCKED", "unavailable", 3, Some("VAT_BLOCKED")),
        ("IP_BLOCKED", "unavailable", 3, Some("IP_BLOCKED")),
    ];
    for (answer, status, exit, code) in cases {
        let body = answer.to_string();
        let mock = Mock::start(move |_| match body.starts_with('{') {
            true => reply(200, body.clone()),
            false => vies_error(&body),
        });
        let (got, v) = mock.json(&["check", "DE136695976", "--live"], &[]);
        let r = first(&v);
        assert_eq!(
            (r["status"].as_str(), got),
            (Some(status), exit),
            "{answer}"
        );
        assert_eq!(r["live"]["code"].as_str(), code, "{answer}");
        assert_eq!(r["offline"]["valid"], true);
        assert_eq!(r["error"], Value::Null);
        let checked = if status == "unavailable" {
            "offline"
        } else {
            "live"
        };
        assert_eq!(r["checked"], checked, "{answer}");
    }
    let mock = Mock::start(|_| vies_error("MS_UNAVAILABLE"));
    let (_, out, _) = mock.run(&["check", "DE136695976", "--live"], &[]);
    assert_eq!(
        out,
        "! DE136695976  VAT · DE  unavailable · VIES didn't answer for DE (MS_UNAVAILABLE). This is not a verdict on the number. Try again later.\n  ! Also a valid eori format; pass --type to check it as one.\n"
    );
    let mock = Mock::start(|_| vies_error("INVALID_INPUT"));
    let (_, v) = mock.json(&["check", "DE136695976", "--live"], &[]);
    assert_eq!(
        first(&v)["message"],
        "VIES says this isn't a valid DE VAT number (INVALID_INPUT)."
    );
}

#[test]
fn the_registry_has_the_last_word_over_check_digits() {
    let mock = Mock::start(|_| reply(200, vies_ok("EXAMPLE GMBH")));
    let (code, v) = mock.json(&["check", "DE136695977", "-t", "vat", "--live"], &[]);
    let r = first(&v);
    assert_eq!((code, r["status"].as_str()), (0, Some("valid")));
    assert_eq!(r["offline"]["valid"], false);
    assert_eq!(r["error"], Value::Null);
    assert_eq!(
        r["warnings"][0],
        json!({"code": "check_digits_disagree", "message": "Our check digits say invalid, but VIES has this number registered. Please report it at github.com/nowmyagentcan/validate-identifier/issues."})
    );
    // Without an answer, the failed check digits stand on their own.
    let mock = Mock::start(|_| vies_error("MS_UNAVAILABLE"));
    let (code, v) = mock.json(&["check", "DE136695977", "-t", "vat", "--live"], &[]);
    let r = first(&v);
    assert_eq!((code, r["status"].as_str()), (1, Some("invalid")));
    assert_eq!(r["checked"], "offline");
    assert_eq!(r["error"]["code"], "vat_check_digits");
    assert_eq!(
        r["message"],
        "The check digits don't match for a DE VAT number; VIES didn't answer either."
    );
    assert_eq!(r["live"]["status"], "unavailable");
}

#[test]
fn asks_nothing_for_bad_formats_or_types_without_a_source() {
    let mock = Mock::start(|_| reply(500, ""));
    let (code, v) = mock.json(
        &[
            "check",
            "DE13669597",
            "DE44500105175407324931",
            "+44 20 7946 0958",
            "--live",
        ],
        &[],
    );
    assert_eq!(code, 1);
    assert!(mock.requests().is_empty());
    for r in v["results"].as_array().unwrap() {
        assert_eq!(
            (r["live"].clone(), r["checked"].as_str()),
            (Value::Null, Some("offline"))
        );
    }
}

#[test]
fn asks_once_for_repeated_values() {
    let mock = Mock::start(|_| reply(200, vies_ok("EXAMPLE GMBH")));
    let (code, v) = mock.json(
        &[
            "check",
            "DE136695976",
            "de 136695976",
            "DE136695976",
            "--live",
        ],
        &[],
    );
    assert_eq!(code, 0);
    assert_eq!(mock.requests().len(), 1);
    assert_eq!(v["summary"]["valid"], 3);
    assert_eq!(v["results"][1]["input"], "de 136695976");
}

#[test]
fn prints_the_registry_stamp_and_name() {
    let mock = Mock::start(|_| reply(200, vies_ok("EXAMPLE GMBH")));
    let (code, out, err) = mock.run(&["check", "DE136695976", "--type", "vat", "--live"], &[]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(
        out,
        "✓ DE136695976  VAT · DE  valid · VIES 2026-10-29 10:12 UTC · EXAMPLE GMBH\n"
    );
}

#[test]
fn http_failures_are_unavailable_with_the_offline_result_kept() {
    let cases: [(Answer, &str); 3] = [
        (
            Answer::Reply(
                302,
                vec![("Location", "https://example.org/".into())],
                String::new(),
            ),
            "VIES redirected the request (HTTP 302). This is not a verdict on the number.",
        ),
        (
            reply(200, "x".repeat(1_100_000)),
            "VIES sent more data than expected.",
        ),
        (
            reply(200, "<html>maintenance</html>"),
            "VIES sent a response we couldn't read.",
        ),
    ];
    for (answer, message) in cases {
        let answer = std::sync::Mutex::new(Some(answer));
        let mock = Mock::start(move |_| answer.lock().unwrap().take().unwrap_or(reply(500, "")));
        let (code, v) = mock.json(&["check", "DE136695976", "--live"], &[]);
        let r = first(&v);
        assert_eq!(
            (code, r["status"].as_str()),
            (3, Some("unavailable")),
            "{message}"
        );
        assert_eq!(r["message"], message);
        assert_eq!(r["offline"]["valid"], true);
    }
}

#[test]
fn an_unreachable_registry_is_unavailable_never_invalid() {
    let url = closed_port();
    let env: Vec<(&str, &str)> = common::SOURCES.iter().map(|&v| (v, url.as_str())).collect();
    let (code, out, _) = vi(&["check", "DE136695976", "--live", "--json"], &env);
    assert_eq!(code, 3);
    let v: Value = serde_json::from_str(&out).unwrap();
    let r = first(&v);
    assert_eq!(r["status"], "unavailable");
    assert!(
        r["message"]
            .as_str()
            .unwrap()
            .starts_with("Couldn't reach 127.0.0.1: "),
        "{r}"
    );
    assert!(
        r["message"]
            .as_str()
            .unwrap()
            .ends_with(". This is not a verdict on the number.")
    );
}

#[test]
fn turns_away_plain_http_overrides_to_other_hosts() {
    let (code, out, err) = vi(
        &["check", "DE136695976", "--live"],
        &[("VALIDATE_IDENTIFIER_VIES_URL", "http://example.org")],
    );
    assert_eq!((code, out.as_str()), (2, ""));
    assert_eq!(
        err,
        "✗ VALIDATE_IDENTIFIER_VIES_URL must start with https:// (http:// only for 127.0.0.1, [::1] or localhost).\n  Fix it, or unset it to use the official address.\n"
    );
}
