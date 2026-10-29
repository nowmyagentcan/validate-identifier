//! Live checks through the binary against the loopback mock: every documented answer, the
//! exact requests, and that a failed request is never a verdict.

mod common;

use common::{Answer, GUID, HMRC_CREDS, Mock, TOKEN, closed_port, reply, vi};
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
        let mock = in_turn(vec![answer], || reply(500, ""));
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

/// The request start times `--debug` printed for `source` (to the ms, so gaps may round
/// down by 1 ms).
fn starts(err: &str, source: &str) -> Vec<f64> {
    err.lines()
        .filter(|l| l.starts_with(&format!("debug: {source} ")) && l.ends_with(" s)"))
        .map(|l| {
            let at = l.rsplit("(+").next().unwrap();
            at.trim_end_matches(" s)").parse().unwrap()
        })
        .collect()
}

#[test]
fn paces_requests_to_each_source() {
    let mock = Mock::start(|_| reply(200, vies_ok("EXAMPLE GMBH")));
    let (code, _, err) = mock.run(
        &[
            "check",
            "DE136695976",
            "IE6388047V",
            "FR0H384498879",
            "--live",
            "--debug",
        ],
        &[],
    );
    assert_eq!(code, 0);
    let at = starts(&err, "VIES");
    assert_eq!(at.len(), 3, "{err}");
    assert!(at.windows(2).all(|w| w[1] - w[0] >= 0.999), "{at:?}");
    let got = mock.requests();
    assert!(
        got.windows(2)
            .all(|w| w[1].at - w[0].at >= std::time::Duration::from_millis(990))
    );
}

/// A mock that gives `first` answers in turn, then `then` forever.
fn in_turn(first: Vec<Answer>, then: impl Fn() -> Answer + Send + 'static) -> Mock {
    let first = std::sync::Mutex::new(first.into_iter());
    Mock::start(move |_| first.lock().unwrap().next().unwrap_or_else(&then))
}

#[test]
fn retries_once_after_a_server_error_or_a_dropped_connection() {
    for failure in [reply(503, ""), Answer::Close] {
        let mock = in_turn(vec![failure], || reply(200, vies_ok("EXAMPLE GMBH")));
        let (code, v) = mock.json(&["check", "DE136695976", "--live"], &[]);
        assert_eq!((code, first(&v)["status"].as_str()), (0, Some("valid")));
        let got = mock.requests();
        assert_eq!(got.len(), 2);
        assert!(got[1].at - got[0].at >= std::time::Duration::from_secs(2));
    }
    // A second failure is the answer.
    let mock = Mock::start(|_| reply(502, ""));
    let (code, v) = mock.json(&["check", "DE136695976", "--live"], &[]);
    assert_eq!((code, mock.requests().len()), (3, 2));
    assert_eq!(
        first(&v)["message"],
        "VIES answered (HTTP 502). This is not a verdict on the number."
    );
    assert_eq!(first(&v)["live"]["code"], "502");
}

#[test]
fn never_retries_a_timeout() {
    let mock = Mock::start(|_| Answer::Hang);
    let (code, v) = mock.json(&["check", "DE136695976", "--live"], &[]);
    assert_eq!((code, mock.requests().len()), (3, 1));
    assert_eq!(first(&v)["message"], "VIES took longer than 20 s.");
}

#[test]
fn honours_short_retry_after_and_reports_long_ones() {
    let busy = |secs: &str| Answer::Reply(429, vec![("Retry-After", secs.into())], String::new());
    let mock = in_turn(vec![busy("1")], || reply(200, vies_ok("EXAMPLE GMBH")));
    let (code, _) = mock.json(&["check", "DE136695976", "--live"], &[]);
    assert_eq!((code, mock.requests().len()), (0, 2));
    let mock = Mock::start(move |_| busy("60"));
    let (code, v) = mock.json(&["check", "DE136695976", "--live"], &[]);
    let r = first(&v);
    assert_eq!((code, mock.requests().len()), (3, 1));
    assert_eq!(r["status"], "rate_limited");
    assert_eq!(
        r["message"],
        "VIES asked us to slow down (429). Try again in 60 s."
    );
    assert_eq!(
        (
            r["live"]["code"].as_str(),
            r["live"]["retry_after"].as_u64()
        ),
        (Some("429"), Some(60))
    );
}

#[test]
fn waits_twice_when_vies_is_busy() {
    let mock = Mock::start(|_| vies_error("MS_MAX_CONCURRENT_REQ"));
    let (code, v) = mock.json(&["check", "DE136695976", "--live"], &[]);
    let r = first(&v);
    assert_eq!((code, r["status"].as_str()), (3, Some("rate_limited")));
    assert_eq!(
        r["message"],
        "VIES is busy (MS_MAX_CONCURRENT_REQ). Try again in a minute."
    );
    assert_eq!(r["live"]["retry_after"], 60);
    let got = mock.requests();
    assert_eq!(got.len(), 3);
    assert!(got[1].at - got[0].at >= std::time::Duration::from_secs(2));
    assert!(got[2].at - got[1].at >= std::time::Duration::from_secs(4));
}

#[test]
fn skips_a_country_after_two_failures_in_a_row() {
    let mock = Mock::start(|r| match r.json()["countryCode"].as_str() {
        Some("DE") => vies_error("MS_UNAVAILABLE"),
        _ => reply(200, vies_ok("EXAMPLE SARL")),
    });
    let values = ["DE136695976", "DE129273398", "DE811569869", "FR0H384498879"];
    let (code, v) = mock.json(
        &[&["check"][..], &values, &["-t", "vat", "--live"]].concat(),
        &[],
    );
    assert_eq!(code, 3);
    let sent: Vec<String> = mock
        .requests()
        .iter()
        .map(|r| r.json()["vatNumber"].to_string())
        .collect();
    assert_eq!(sent, ["\"136695976\"", "\"129273398\"", "\"0H384498879\""]);
    let r = &v["results"];
    assert_eq!(
        r[2]["message"],
        "Skipped: VIES (DE) failed twice in a row in this run. This is not a verdict on the number."
    );
    assert_eq!(
        (r[2]["status"].as_str(), r[3]["status"].as_str()),
        (Some("unavailable"), Some("valid"))
    );
}

#[test]
fn skips_a_source_it_cannot_reach() {
    let mock = Mock::start(|_| Answer::Close);
    let (code, v) = mock.json(
        &[
            "check",
            "DE136695976",
            "IE6388047V",
            "FR0H384498879",
            "-t",
            "vat",
            "--live",
        ],
        &[],
    );
    assert_eq!((code, mock.requests().len()), (3, 4));
    assert_eq!(
        v["results"][2]["message"],
        "Skipped: VIES failed twice in a row in this run. This is not a verdict on the number."
    );
}

#[test]
fn holds_the_live_limit() {
    let values = vec!["DE136695976"; 1001];
    let (code, out, err) = vi(&[&["check", "--live"][..], &values].concat(), &[]);
    assert_eq!((code, out.as_str()), (2, ""));
    assert_eq!(
        err,
        "✗ 1001 values; at most 1,000 per run with --live.\n  Split the list, or check it offline first.\n"
    );
}

#[test]
fn sends_the_requester_to_vies_for_a_consultation_number() {
    let mock = Mock::start(|_| {
        let mut ok: Value = serde_json::from_str(&vies_ok("EXAMPLE LTD")).unwrap();
        ok["requestIdentifier"] = "WAPIAAAAZ1QX4Q2B".into();
        reply(200, ok.to_string())
    });
    let (code, v) = mock.json(
        &[
            "check",
            "IE6388047V",
            "-t",
            "vat",
            "--live",
            "--requester",
            "de 136 695 976",
        ],
        &[],
    );
    assert_eq!(code, 0);
    assert_eq!(
        mock.requests()[0].json(),
        json!({"countryCode": "IE", "vatNumber": "6388047V",
               "requesterMemberStateCode": "DE", "requesterNumber": "136695976"})
    );
    assert_eq!(
        first(&v)["live"]["data"]["consultation_number"],
        "WAPIAAAAZ1QX4Q2B"
    );
    assert_eq!(first(&v)["warnings"], json!([]));

    // A UK or Swiss requester isn't sent to VIES.
    let (_, v) = mock.json(
        &[
            "check",
            "IE6388047V",
            "-t",
            "vat",
            "--live",
            "--requester",
            "GB434031439",
        ],
        &[],
    );
    assert!(mock.requests()[1].json().get("requesterNumber").is_none());
    assert_eq!(
        first(&v)["warnings"],
        json!([{"code": "requester_not_used", "message": "--requester is a GB number, so it wasn't sent to VIES."}])
    );
}

#[test]
fn stops_when_vies_rejects_the_requester() {
    let mock = Mock::start(|_| vies_error("INVALID_REQUESTER_INFO"));
    let (code, out, err) = mock.run(
        &[
            "check",
            "IE6388047V",
            "DE136695976",
            "--live",
            "--requester",
            "DE136695976",
        ],
        &[],
    );
    assert_eq!((code, out.as_str()), (2, ""));
    assert_eq!(
        err,
        "✗ VIES rejected your --requester number.\n  Pass your own valid VAT number, or leave out --requester.\n"
    );
    assert_eq!(mock.requests().len(), 1);
    let (code, _, err) = vi(&["check", "IE6388047V", "--requester", "DE136695977"], &[]);
    assert_eq!(code, 2);
    assert_eq!(
        err,
        "✗ --requester DE136695977 isn't a valid VAT number.\n  Pass your own VAT number with its country prefix, e.g. DE136695976.\n"
    );
}

/// A mock HMRC that hands out a token, then answers lookups with `lookup(path)`.
fn hmrc(lookup: impl Fn(&str) -> Answer + Send + 'static) -> Mock {
    Mock::start(move |r| {
        match r.target.as_str() {
        "/oauth/token" => reply(
            200,
            json!({"access_token": TOKEN, "token_type": "bearer", "expires_in": 14400, "scope": "read:vat"})
                .to_string(),
        ),
        path => lookup(path),
    }
    })
}

fn hmrc_ok() -> Answer {
    reply(
        200,
        json!({
            "target": {"name": "EXAMPLE TRADING LTD", "vatNumber": "434031439",
                       "address": {"line1": "1 Test Road", "line2": "Testville", "postcode": "TE1 1ST", "countryCode": "GB"}},
            "processingDate": "2026-10-29T11:12:03+01:00",
            "consultationNumber": "abcDEF123",
        })
        .to_string(),
    )
}

#[test]
fn says_uk_vat_needs_hmrc_credentials() {
    let mock = Mock::start(|_| reply(500, ""));
    let (code, out, _) = mock.run(&["check", "GB434031439", "GB434031439001", "--live"], &[]);
    assert_eq!(code, 3);
    assert!(mock.requests().is_empty());
    assert_eq!(
        out.lines().take(2).collect::<Vec<_>>(),
        [
            "! GB434031439     VAT · GB  not configured · UK VAT checks need your own HMRC credentials.",
            "                            Set HMRC_CLIENT_ID and HMRC_CLIENT_SECRET (see the README)."
        ]
    );
    let (_, v) = mock.json(&["check", "GB434031439", "--live"], &[]);
    let r = first(&v);
    assert_eq!(
        (r["status"].as_str(), r["checked"].as_str()),
        (Some("not_configured"), Some("offline"))
    );
    assert_eq!(
        (r["live"]["source"].as_str(), r["live"]["status"].as_str()),
        (Some("HMRC"), Some("not_configured"))
    );
}

#[test]
fn asks_hmrc_with_a_token_fetched_once() {
    let mock = hmrc(|_| hmrc_ok());
    let (code, v) = mock.json(
        &["check", "GB434031439", "GB 434 0314 39001", "--live"],
        &HMRC_CREDS,
    );
    assert_eq!(code, 0);
    let got = mock.requests();
    assert_eq!(got.len(), 3);
    assert_eq!(
        (got[0].method.as_str(), got[0].target.as_str()),
        ("POST", "/oauth/token")
    );
    assert_eq!(
        got[0].header("content-type"),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(
        got[0].body,
        "client_id=id-abc123&client_secret=s3cret%2Fxyz&grant_type=client_credentials&scope=read%3Avat"
    );
    for lookup in &got[1..] {
        assert_eq!(lookup.method, "GET");
        assert_eq!(
            lookup.target,
            "/organisations/vat/check-vat-number/lookup/434031439"
        );
        assert_eq!(
            lookup.header("accept"),
            Some("application/vnd.hmrc.2.0+json")
        );
        assert_eq!(
            lookup.header("authorization"),
            Some(format!("Bearer {TOKEN}").as_str())
        );
        assert_eq!(lookup.header("user-agent"), Some(UA));
    }
    assert!(got[2].at - got[1].at >= std::time::Duration::from_millis(390));
    let r = first(&v);
    assert_eq!(r["status"], "valid");
    assert_eq!(
        r["message"],
        "Registered for VAT (HMRC, 2026-10-29 10:12 UTC)."
    );
    assert_eq!(
        r["live"],
        json!({"source": "HMRC", "status": "active", "checked_at": "2026-10-29T10:12:03Z", "code": null,
               "retry_after": null, "data": {"name": "EXAMPLE TRADING LTD",
               "address": "1 Test Road, Testville, TE1 1ST", "consultation_number": "abcDEF123"}})
    );
    // The branch suffix stays in the details only.
    assert_eq!(v["results"][1]["details"]["branch"], "001");
}

#[test]
fn sends_a_uk_requester_to_hmrc_only() {
    let mock = hmrc(|_| hmrc_ok());
    let (code, v) = mock.json(
        &[
            "check",
            "GB434031439",
            "--live",
            "--requester",
            "GB434031439",
        ],
        &HMRC_CREDS,
    );
    assert_eq!(code, 0);
    assert_eq!(
        mock.requests()[1].target,
        "/organisations/vat/check-vat-number/lookup/434031439/434031439"
    );
    assert_eq!(first(&v)["warnings"], json!([]));
    let (_, v) = mock.json(
        &[
            "check",
            "GB434031439",
            "--live",
            "--requester",
            "DE136695976",
        ],
        &HMRC_CREDS,
    );
    assert_eq!(
        mock.requests()[3].target,
        "/organisations/vat/check-vat-number/lookup/434031439"
    );
    assert_eq!(
        first(&v)["warnings"],
        json!([{"code": "requester_not_used", "message": "--requester is a DE number, so it wasn't sent to HMRC."}])
    );
    let mock = hmrc(|_| {
        reply(403, json!({"code": "INVALID_REQUEST", "message": "requesterVrn does not match a registered company"}).to_string())
    });
    let (code, out, err) = mock.run(
        &[
            "check",
            "GB434031439",
            "--live",
            "--requester",
            "GB434031439",
        ],
        &HMRC_CREDS,
    );
    assert_eq!((code, out.as_str()), (2, ""));
    assert_eq!(
        err,
        "✗ HMRC rejected your --requester number.\n  Pass your own valid VAT number, or leave out --requester.\n"
    );
}

#[test]
fn maps_every_hmrc_answer() {
    let error = |status: u16, code: &str| {
        let body = json!({"code": code, "message": "x"}).to_string();
        move |_: &str| reply(status, body.clone())
    };
    let not_found = hmrc(error(404, "NOT_FOUND"));
    let (code, v) = not_found.json(&["check", "GB434031439", "--live"], &HMRC_CREDS);
    assert_eq!((code, first(&v)["status"].as_str()), (1, Some("not_found")));
    assert_eq!(
        first(&v)["message"],
        "HMRC has no VAT registration for this number."
    );
    assert_eq!(first(&v)["live"]["code"], "NOT_FOUND");

    let bad = hmrc(error(400, "INVALID_REQUEST"));
    let (code, v) = bad.json(&["check", "GB434031439", "--live"], &HMRC_CREDS);
    assert_eq!(
        (code, first(&v)["status"].as_str()),
        (3, Some("unavailable"))
    );
    assert_eq!(
        first(&v)["message"],
        "HMRC didn't accept the request (INVALID_REQUEST). This is not a verdict on the number."
    );

    let busy = hmrc(|_| {
        Answer::Reply(
            429,
            vec![("Retry-After", "45".into())],
            r#"{"code":"MESSAGE_THROTTLED_OUT"}"#.into(),
        )
    });
    let (code, v) = busy.json(&["check", "GB434031439", "--live"], &HMRC_CREDS);
    assert_eq!(
        (code, first(&v)["status"].as_str()),
        (3, Some("rate_limited"))
    );
    assert_eq!(first(&v)["live"]["retry_after"], 45);

    let expired = hmrc(error(401, "INVALID_CREDENTIALS"));
    let (code, v) = expired.json(&["check", "GB434031439", "--live"], &HMRC_CREDS);
    assert_eq!(
        (code, first(&v)["status"].as_str()),
        (3, Some("not_configured"))
    );
    assert_eq!(
        first(&v)["message"],
        "HMRC rejected the client ID or secret."
    );

    let down = hmrc(error(503, "SERVER_ERROR"));
    let (code, v) = down.json(&["check", "GB434031439", "--live"], &HMRC_CREDS);
    assert_eq!(
        (code, first(&v)["status"].as_str()),
        (3, Some("unavailable"))
    );
    assert_eq!(down.requests().len(), 3, "token, lookup, one retry");

    // Rejected credentials: said once, and nothing more is asked.
    let rejected = Mock::start(|_| reply(401, r#"{"error":"invalid_client"}"#));
    let (code, out, _) = rejected.run(
        &["check", "GB434031439", "GB980780684", "--live"],
        &HMRC_CREDS,
    );
    assert_eq!((code, rejected.requests().len()), (3, 1));
    assert!(out.starts_with(
        "! GB434031439  VAT · GB  not configured · HMRC rejected the client ID or secret.\n                         Check HMRC_CLIENT_ID, HMRC_CLIENT_SECRET and VALIDATE_IDENTIFIER_HMRC_URL (sandbox credentials need the test URL).\n"
    ), "{out}");
}

#[test]
fn never_prints_secrets_even_with_debug() {
    // Every run against the mock also checks its output for the test secrets.
    let mock = hmrc(|path| match path.starts_with("/json/") {
        true => abn_answer("51824753556", "Active", ""),
        false => hmrc_ok(),
    });
    let env = [HMRC_CREDS[0], HMRC_CREDS[1], GUID];
    for args in [&["--json"][..], &[], &["--debug"]] {
        let (code, _, err) = mock.run(
            &[&["check", "GB434031439", "51824753556", "--live"][..], args].concat(),
            &env,
        );
        assert_eq!(code, 0);
        if args == ["--debug"] {
            assert!(err.contains("&guid=REDACTED "), "{err}");
        }
    }
}

/// A `GetByUID` answer in the register's shape (eCH-0108): `status` is the UID status code,
/// `vat` the VAT block's status and entry status codes.
fn uid_answer(status: &str, vat: Option<(&str, &str)>) -> Answer {
    let vat = vat.map_or(String::new(), |(s, e)| format!(
        "<vatRegisterInformation xmlns=\"http://www.ech.ch/xmlns/eCH-0108/5\"><vatStatus>{s}</vatStatus><vatEntryStatus>{e}</vatEntryStatus><vatEntryDate>1995-01-01</vatEntryDate></vatRegisterInformation>"
    ));
    reply(
        200,
        format!(
            r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><GetByUIDResponse xmlns="http://www.uid.admin.ch/xmlns/uid-wse"><GetByUIDResult><organisationType><organisation xmlns="http://www.ech.ch/xmlns/eCH-0108/5"><organisationIdentification xmlns="http://www.ech.ch/xmlns/eCH-0098/5"><organisationName xmlns="http://www.ech.ch/xmlns/eCH-0097/5">Beispiel AG</organisationName></organisationIdentification></organisation><uidregInformation xmlns="http://www.ech.ch/xmlns/eCH-0108/5"><uidregStatusEnterpriseDetail>{status}</uidregStatusEnterpriseDetail></uidregInformation>{vat}</organisationType></GetByUIDResult></GetByUIDResponse></s:Body></s:Envelope>"#
        ),
    )
}

fn soap_fault(text: &str) -> Answer {
    reply(
        500,
        format!(
            r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><s:Fault><faultcode>s:Client</faultcode><faultstring xml:lang="de-CH">{text}</faultstring></s:Fault></s:Body></s:Envelope>"#
        ),
    )
}

#[test]
fn asks_the_uid_register_with_the_documented_soap_call() {
    let mock = Mock::start(|_| uid_answer("3", Some(("2", "1"))));
    let (code, v) = mock.json(
        &["check", "CHE-116.281.710", "che116281710mwst", "--live"],
        &[],
    );
    assert_eq!(code, 0);
    let got = mock.requests();
    assert_eq!(got.len(), 2);
    assert!(got[1].at - got[0].at >= std::time::Duration::from_millis(3190));
    let req = &got[0];
    assert_eq!(
        (req.method.as_str(), req.target.as_str()),
        ("POST", "/V5.0/PublicServices.svc")
    );
    assert_eq!(req.header("content-type"), Some("text/xml; charset=utf-8"));
    assert_eq!(
        req.header("soapaction"),
        Some("\"http://www.uid.admin.ch/xmlns/uid-wse/IPublicServices/GetByUID\"")
    );
    assert_eq!(
        req.body,
        r#"<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/" xmlns:uid="http://www.uid.admin.ch/xmlns/uid-wse" xmlns:ech="http://www.ech.ch/xmlns/eCH-0097/5"><soapenv:Body><uid:GetByUID><uid:uid><ech:uidOrganisationIdCategorie>CHE</ech:uidOrganisationIdCategorie><ech:uidOrganisationId>116281710</ech:uidOrganisationId></uid:uid></uid:GetByUID></soapenv:Body></soapenv:Envelope>"#
    );
    assert_eq!(
        first(&v)["live"]["data"],
        json!({"name": "Beispiel AG", "uid_status": "active", "vat_status": "registered",
               "vat_active": true, "vat_since": "1995-01-01"})
    );
    assert!(
        first(&v)["message"]
            .as_str()
            .unwrap()
            .starts_with("Registered in the UID register (")
    );
    assert!(
        v["results"][1]["message"]
            .as_str()
            .unwrap()
            .starts_with("Registered for VAT (UID register, ")
    );
}

#[test]
fn maps_every_uid_register_answer() {
    let empty = || {
        reply(
            200,
            r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><GetByUIDResponse xmlns="http://www.uid.admin.ch/xmlns/uid-wse"><GetByUIDResult/></GetByUIDResponse></s:Body></s:Envelope>"#,
        )
    };
    let uid = "CHE-116.281.710";
    let vat = "CHE-116.281.710 MWST";
    let cases: Vec<(&str, Answer, &str, i32)> = vec![
        (uid, uid_answer("1", None), "valid", 0),
        (uid, uid_answer("2", None), "valid", 0),
        (uid, uid_answer("4", None), "valid", 0),
        (uid, uid_answer("5", None), "inactive", 1),
        (uid, uid_answer("6", None), "inactive", 1),
        (uid, uid_answer("7", None), "inactive", 1),
        (uid, empty(), "not_found", 1),
        (vat, uid_answer("3", Some(("2", "2"))), "inactive", 1),
        (vat, uid_answer("3", Some(("3", "1"))), "not_found", 1),
        (vat, uid_answer("3", None), "not_found", 1),
        (vat, uid_answer("3", Some(("1", "1"))), "unavailable", 3),
        (vat, empty(), "not_found", 1),
        (uid, soap_fault("Data_validation_failed"), "unavailable", 3),
        (uid, reply(200, "not xml"), "unavailable", 3),
    ];
    for (value, answer, status, exit) in cases {
        let mock = in_turn(vec![answer], || reply(500, ""));
        let (code, v) = mock.json(&["check", value, "--live"], &[]);
        assert_eq!(
            (first(&v)["status"].as_str(), code),
            (Some(status), exit),
            "{value} {v}"
        );
    }
    let mock = Mock::start(|_| uid_answer("7", None));
    let (_, v) = mock.json(&["check", uid, "--live"], &[]);
    assert_eq!(
        first(&v)["message"],
        "This UID is annulled in the UID register."
    );
    assert_eq!(first(&v)["live"]["data"]["vat_status"], Value::Null);
    let mock = Mock::start(|_| soap_fault("Data_validation_failed"));
    let (_, v) = mock.json(&["check", uid, "--live"], &[]);
    assert_eq!(first(&v)["live"]["code"], "Data_validation_failed");
    assert_eq!(mock.requests().len(), 1, "a fault isn't retried");
}

#[test]
fn reports_the_uid_register_limit() {
    let mock = Mock::start(|_| soap_fault("Request_limit_exceeded"));
    let (code, out, _) = mock.run(&["check", "CHE-116.281.710 MWST", "--live"], &[]);
    assert_eq!((code, mock.requests().len()), (3, 1));
    assert_eq!(
        out,
        "! CHE-116.281.710 MWST  VAT · CH  rate limited · The UID register limits checks to 20 per minute and asked us to wait. Try again in a minute.\n"
    );
    let (_, v) = mock.json(&["check", "CHE-116.281.710", "--live"], &[]);
    let live = &first(&v)["live"];
    assert_eq!(
        (live["code"].as_str(), live["retry_after"].as_u64()),
        (Some("Request_limit_exceeded"), Some(60))
    );
}

fn abn_answer(abn: &str, status: &str, message: &str) -> Answer {
    let body = json!({
        "Abn": abn, "AbnStatus": status, "AbnStatusEffectiveFrom": "2000-03-14", "Acn": "",
        "AddressDate": "2020-01-01", "AddressPostcode": "2000", "AddressState": "NSW",
        "BusinessName": [], "EntityName": "EXAMPLE PTY LTD", "EntityTypeCode": "PRV",
        "EntityTypeName": "Australian Private Company", "Gst": "2000-07-01", "Message": message
    });
    reply(200, format!("callback({body})"))
}

#[test]
fn asks_abn_lookup_with_the_guid_and_never_shows_it() {
    let mock = Mock::start(|_| abn_answer("51824753556", "Active", ""));
    let (code, v) = mock.json(
        &["check", "51 824 753 556", "53004085616", "--live"],
        &[GUID],
    );
    assert_eq!(code, 0);
    let got = mock.requests();
    assert!(got[1].at - got[0].at >= std::time::Duration::from_millis(490));
    let req = &got[0];
    assert_eq!(req.method, "GET");
    assert_eq!(
        req.target,
        "/json/AbnDetails.aspx?abn=51824753556&guid=f0e1d2c3-b4a5-9687-7869-5a4b3c2d1e0f"
    );
    assert_eq!(req.header("user-agent"), Some(UA));
    let r = first(&v);
    assert!(
        r["message"]
            .as_str()
            .unwrap()
            .starts_with("Active in ABN Lookup (")
    );
    assert_eq!(
        r["live"]["data"],
        json!({"entity_name": "EXAMPLE PTY LTD", "entity_type": "Australian Private Company",
               "abn_status": "Active", "abn_status_from": "2000-03-14", "gst_from": "2000-07-01",
               "state": "NSW", "postcode": "2000"})
    );
}

#[test]
fn maps_every_abn_lookup_answer() {
    let cases = [
        (
            abn_answer("51824753556", "Cancelled", ""),
            "inactive",
            "ABN Lookup lists this ABN as Cancelled.",
        ),
        (
            abn_answer("", "", "Search text is not a valid ABN or ACN"),
            "not_found",
            "Not found in ABN Lookup.",
        ),
        (
            reply(200, "callback(nonsense"),
            "unavailable",
            "ABN Lookup sent a response we couldn't read.",
        ),
        (
            reply(400, ""),
            "unavailable",
            "ABN Lookup answered (HTTP 400). This is not a verdict on the number.",
        ),
    ];
    for (answer, status, message) in cases {
        let mock = in_turn(vec![answer], || reply(500, ""));
        let (_, v) = mock.json(&["check", "51824753556", "--live"], &[GUID]);
        assert_eq!(
            (first(&v)["status"].as_str(), first(&v)["message"].as_str()),
            (Some(status), Some(message))
        );
    }
    // An unknown GUID is said once; the other ABNs aren't sent.
    let mock = Mock::start(|_| {
        abn_answer(
            "",
            "",
            "The GUID entered is not recognised as a Registered Party",
        )
    });
    let (code, out, _) = mock.run(&["check", "51824753556", "53004085616", "--live"], &[GUID]);
    assert_eq!((code, mock.requests().len()), (3, 1));
    assert!(out.starts_with(
        "! 51824753556  ABN · AU  not configured · ABN Lookup didn't recognise ABN_LOOKUP_GUID.\n                         Set ABN_LOOKUP_GUID (free at abr.business.gov.au/Tools/WebServices).\n"
    ), "{out}");
    // Without a GUID nothing is sent.
    let (code, v) = mock.json(&["check", "51824753556", "--live"], &[]);
    assert_eq!((code, mock.requests().len()), (3, 1));
    assert_eq!(
        first(&v)["message"],
        "ABN checks need your own ABN Lookup GUID."
    );
}

/// A valid LEI ending in `n` (ISO 17442 check digits computed).
fn lei(n: usize) -> String {
    let base = format!("5493{n:014}");
    let rem = format!("{base}00")
        .bytes()
        .fold(0u32, |r, b| (r * 10 + u32::from(b - b'0')) % 97);
    format!("{base}{:02}", 98 - rem)
}

fn gleif_record(lei: &str, registration: &str, entity: &str) -> Value {
    json!({"type": "lei-records", "id": lei, "attributes": {
        "lei": lei,
        "entity": {"legalName": {"name": format!("EXAMPLE {lei}"), "language": "en"}, "status": entity},
        "registration": {"status": registration, "nextRenewalDate": "2027-06-28T18:34:06Z",
                         "managingLou": "5493001KJTIIGC8Y1R12"}
    }})
}

#[test]
fn asks_gleif_in_batches_of_100() {
    let leis: Vec<String> = (0..150).map(lei).collect();
    let mock = Mock::start(|r| {
        // Every LEI the request names, except the tenth, is issued.
        let query = r.target.split("filter%5Blei%5D=").nth(1).unwrap();
        let asked = query.split('&').next().unwrap().split(',');
        let data: Vec<Value> = asked
            .filter(|l| *l != lei(9))
            .map(|l| gleif_record(l, "ISSUED", "ACTIVE"))
            .collect();
        reply(200, json!({"meta": {}, "data": data}).to_string())
    });
    let args: Vec<&str> = leis.iter().map(String::as_str).collect();
    let (code, v) = mock.json(&[&["check", "--live"][..], &args].concat(), &[]);
    assert_eq!(code, 1);
    let got = mock.requests();
    assert_eq!(got.len(), 2);
    assert!(got[1].at - got[0].at >= std::time::Duration::from_millis(1090));
    assert_eq!(
        got[0].target,
        format!(
            "/api/v1/lei-records?filter%5Blei%5D={}&page%5Bsize%5D=100",
            leis[..100].join(",")
        )
    );
    assert_eq!(got[0].header("accept"), Some("application/vnd.api+json"));
    assert_eq!(
        (
            v["summary"]["valid"].as_u64(),
            v["summary"]["not_found"].as_u64()
        ),
        (Some(149), Some(1))
    );
    assert_eq!(v["results"][9]["message"], "Not found in GLEIF.");
    let r = first(&v);
    assert_eq!(
        r["live"]["data"],
        json!({"legal_name": format!("EXAMPLE {}", leis[0]), "entity_status": "ACTIVE",
               "registration_status": "ISSUED", "next_renewal_date": "2027-06-28T18:34:06Z",
               "managing_lou": "5493001KJTIIGC8Y1R12"})
    );
    assert!(
        r["message"]
            .as_str()
            .unwrap()
            .starts_with("Registration ISSUED in GLEIF (")
    );
}

#[test]
fn maps_every_gleif_answer() {
    let one = |registration, entity| {
        reply(
            200,
            json!({"data": [gleif_record(&lei(1), registration, entity)]}).to_string(),
        )
    };
    let busy = Answer::Reply(429, vec![("Retry-After", "45".into())], String::new());
    let cases = [
        (
            one("PENDING_TRANSFER", "ACTIVE"),
            "valid",
            "Registration PENDING_TRANSFER in GLEIF (",
        ),
        (
            one("LAPSED", "ACTIVE"),
            "inactive",
            "GLEIF lists this LEI as LAPSED (entity ACTIVE).",
        ),
        (
            one("ISSUED", "INACTIVE"),
            "inactive",
            "GLEIF lists this LEI as ISSUED (entity INACTIVE).",
        ),
        (reply(404, ""), "not_found", "Not found in GLEIF."),
        (
            busy,
            "rate_limited",
            "GLEIF asked us to slow down (429). Try again in 45 s.",
        ),
        (
            reply(200, "{}"),
            "unavailable",
            "GLEIF sent a response we couldn't read.",
        ),
    ];
    for (answer, status, message) in cases {
        let mock = in_turn(vec![answer], || reply(500, ""));
        let (_, v) = mock.json(&["check", &lei(1), &lei(2), "--live"], &[]);
        let r = first(&v);
        assert_eq!(r["status"], status, "{message}");
        assert!(r["message"].as_str().unwrap().starts_with(message), "{r}");
        // The batch's second LEI shares the answer, or is left out of it.
        let second = v["results"][1]["status"].as_str();
        assert!(second == Some(status) || second == Some("not_found"), "{v}");
        assert_eq!(mock.requests().len(), 1, "{message}");
    }
}

#[test]
fn without_a_network_every_live_check_is_unavailable_and_keeps_its_offline_result() {
    let url = closed_port();
    let mut env: Vec<(&str, &str)> = common::SOURCES.iter().map(|&v| (v, url.as_str())).collect();
    env.extend([HMRC_CREDS[0], HMRC_CREDS[1], GUID]);
    let leis = lei(1);
    let values = [
        "DE136695976",
        "GB434031439",
        "CHE-116.281.710",
        "51824753556",
        &leis,
        "XI1234567890001",
        "GB2204302310001",
    ];
    let (code, out, _) = vi(
        &[
            &["check", "--live", "--json", "DE44500105175407324931"][..],
            &values,
        ]
        .concat(),
        &env,
    );
    assert_eq!(code, 3);
    let v: Value = serde_json::from_str(&out).unwrap();
    let results = v["results"].as_array().unwrap();
    assert_eq!(results[0]["status"], "valid");
    let sources: Vec<&str> = results[1..]
        .iter()
        .map(|r| r["live"]["source"].as_str().unwrap())
        .collect();
    assert_eq!(
        sources,
        [
            "VIES",
            "HMRC",
            "UID register",
            "ABN Lookup",
            "GLEIF",
            "EOS",
            "HMRC"
        ]
    );
    for r in &results[1..] {
        assert_eq!(
            (r["status"].as_str(), r["offline"]["valid"].as_bool()),
            (Some("unavailable"), Some(true)),
            "{r}"
        );
    }
}

/// An EOS answer with one `result` per EORI the request names: `status(eori)` gives its
/// status code and extra elements.
fn eos(status: impl Fn(&str) -> (&'static str, &'static str) + Send + 'static) -> Mock {
    Mock::start(move |r| {
        let results: String = r
            .body
            .split("<ev:eori>")
            .skip(1)
            .map(|e| {
                let eori = e.split('<').next().unwrap();
                let (code, extra) = status(eori);
                format!("<result><eori>{eori}</eori><status>{code}</status><statusDescr>x</statusDescr>{extra}</result>")
            })
            .collect();
        reply(
            200,
            format!(
                r#"<?xml version='1.0' encoding='UTF-8'?><S:Envelope xmlns:S="http://schemas.xmlsoap.org/soap/envelope/"><S:Body><ns0:validateEORIResponse xmlns:ns0="http://eori.ws.eos.dds.s/"><return><requestDate>29/10/2026</requestDate>{results}</return></ns0:validateEORIResponse></S:Body></S:Envelope>"#
            ),
        )
    })
}

#[test]
fn asks_eos_in_batches_of_10_with_the_documented_soap_call() {
    let eoris: Vec<String> = (0..12).map(|n| format!("DE{n:013}")).collect();
    let mock = eos(|e| match e.ends_with('3') {
        true => ("1", ""),
        false => (
            "0",
            "<name>EXAMPLE GMBH</name><street>1 Teststr.</street><postalCode>10115</postalCode><city>Berlin</city><country>DE</country>",
        ),
    });
    let args: Vec<&str> = eoris.iter().map(String::as_str).collect();
    let (code, v) = mock.json(
        &[&["check", "-t", "eori", "--live"][..], &args].concat(),
        &[],
    );
    assert_eq!(code, 1);
    let got = mock.requests();
    assert_eq!(got.len(), 2);
    assert!(got[1].at - got[0].at >= std::time::Duration::from_millis(990));
    let req = &got[0];
    assert_eq!(
        (req.method.as_str(), req.target.as_str()),
        (
            "POST",
            "/taxation_customs/dds2/eos/validation/services/validation"
        )
    );
    assert_eq!(req.header("soapaction"), Some("\"\""));
    assert_eq!(req.header("content-type"), Some("text/xml; charset=utf-8"));
    let list: String = eoris[..10]
        .iter()
        .map(|e| format!("<ev:eori>{e}</ev:eori>"))
        .collect();
    assert_eq!(
        req.body,
        format!(
            r#"<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/" xmlns:ev="http://eori.ws.eos.dds.s/"><soapenv:Body><ev:validateEORI>{list}</ev:validateEORI></soapenv:Body></soapenv:Envelope>"#
        )
    );
    assert!(
        got[1]
            .body
            .contains("<ev:eori>DE0000000000011</ev:eori></ev:validateEORI>")
    );
    let r = first(&v);
    assert_eq!(
        (r["status"].as_str(), r["checked"].as_str()),
        (Some("valid"), Some("live"))
    );
    assert!(
        r["message"]
            .as_str()
            .unwrap()
            .starts_with("Valid EORI number (EOS, ")
    );
    assert_eq!(
        r["live"]["data"],
        json!({"name": "EXAMPLE GMBH", "address": "1 Teststr., 10115, Berlin, DE"})
    );
    let fourth = &v["results"][3];
    assert_eq!(fourth["status"], "not_found");
    assert_eq!(
        fourth["message"],
        "EOS has no EORI registration for this number."
    );
    assert_eq!(
        fourth["live"]["data"],
        json!({"name": null, "address": null})
    );
}

#[test]
fn maps_every_eos_failure_to_unavailable() {
    let soap = |inner: &str| {
        format!(
            r#"<S:Envelope xmlns:S="http://schemas.xmlsoap.org/soap/envelope/"><S:Body>{inner}</S:Body></S:Envelope>"#
        )
    };
    let answer = |body: &str| {
        format!(
            r#"<ns0:validateEORIResponse xmlns:ns0="http://eori.ws.eos.dds.s/"><return><requestDate>29/10/2026</requestDate>{body}</return></ns0:validateEORIResponse>"#
        )
    };
    let cases = [
        (
            reply(
                500,
                soap(
                    "<S:Fault><faultcode>S:Server</faultcode><faultstring>Internal error</faultstring></S:Fault>",
                ),
            ),
            "EOS answered with an error (Internal error). This is not a verdict on the number.",
            Some("Internal error"),
        ),
        (
            reply(
                200,
                soap(&answer(
                    "<errorDescription>Service temporarily unavailable</errorDescription>",
                )),
            ),
            "EOS couldn't check these numbers (Service temporarily unavailable). This is not a verdict on the number.",
            None,
        ),
        (
            reply(
                200,
                soap(&answer(
                    "<result><eori>IE8256796U</eori><status>2</status><statusDescr>Error</statusDescr><errorReason>Backend down</errorReason></result>",
                )),
            ),
            "EOS couldn't check this number (Backend down). This is not a verdict on the number.",
            None,
        ),
        (
            Answer::Reply(
                302,
                vec![("Location", "https://sorry.ec.europa.eu/".into())],
                String::new(),
            ),
            "EOS is down for maintenance (HTTP 302). This is not a verdict on the number.",
            Some("302"),
        ),
        (
            reply(200, soap(&answer(""))),
            "EOS sent a response we couldn't read.",
            None,
        ),
    ];
    for (answer, message, code) in cases {
        let answer = std::sync::Mutex::new(Some(answer));
        let mock = Mock::start(move |_| answer.lock().unwrap().take().unwrap_or(reply(500, "")));
        let (exit, v) = mock.json(&["check", "IE8256796U", "-t", "eori", "--live"], &[]);
        let r = first(&v);
        assert_eq!(
            (exit, r["status"].as_str()),
            (3, Some("unavailable")),
            "{message}"
        );
        assert_eq!(r["message"], message);
        assert_eq!(r["live"]["code"].as_str(), code, "{message}");
        assert_eq!(r["offline"]["valid"], true);
    }
}

#[test]
fn sends_xi_eoris_to_eos_and_gb_eoris_to_hmrc() {
    let mock = Mock::start(|r| match r.target.as_str() {
        "/customs/eori/lookup/check-multiple-eori" => {
            let eoris = r.json()["eoris"].as_array().unwrap().clone();
            let answers: Vec<Value> = eoris
                .iter()
                .map(|e| match e.as_str().unwrap() {
                    "GB220430231000" => json!({"eori": e, "valid": true, "processingDate": "2026-10-29T14:17:13+00:00",
                        "companyDetails": {"traderName": "EXAMPLE LTD",
                            "address": {"streetAndNumber": "1 Test Road", "cityName": "Testville", "postcode": "TE1 1ST"}}}),
                    other => json!({"eori": e, "valid": other != "GB123456789000", "processingDate": "2026-10-29T14:17:13+00:00"}),
                })
                .collect();
            reply(200, Value::from(answers).to_string())
        }
        _ => reply(
            200,
            r#"<S:Envelope xmlns:S="http://schemas.xmlsoap.org/soap/envelope/"><S:Body><return><result><eori>XI123456789000</eori><status>0</status></result></return></S:Body></S:Envelope>"#,
        ),
    });
    // No HMRC credentials: the EORI check is open to all, while UK VAT needs them.
    let (code, v) = mock.json(
        &[
            "check",
            "GB220430231000",
            "XI123456789000",
            "GB123456789000",
            "-t",
            "eori",
            "--live",
        ],
        &[],
    );
    assert_eq!(code, 1);
    let got = mock.requests();
    assert_eq!(got.len(), 2);
    let hmrc = got
        .iter()
        .find(|r| r.target.starts_with("/customs"))
        .unwrap();
    assert_eq!(hmrc.method, "POST");
    assert_eq!(hmrc.header("accept"), Some("application/vnd.hmrc.1.0+json"));
    assert_eq!(hmrc.header("authorization"), None);
    assert_eq!(
        hmrc.json(),
        json!({"eoris": ["GB220430231000", "GB123456789000"]})
    );
    let r = &v["results"];
    assert_eq!(
        (r[0]["live"]["source"].as_str(), r[0]["status"].as_str()),
        (Some("HMRC"), Some("valid"))
    );
    assert_eq!(
        r[0]["message"],
        "Valid EORI number (HMRC, 2026-10-29 14:17 UTC)."
    );
    assert_eq!(r[0]["live"]["checked_at"], "2026-10-29T14:17:13Z");
    assert_eq!(
        r[0]["live"]["data"],
        json!({"name": "EXAMPLE LTD", "address": "1 Test Road, Testville, TE1 1ST"})
    );
    assert_eq!(
        (r[1]["live"]["source"].as_str(), r[1]["status"].as_str()),
        (Some("EOS"), Some("valid"))
    );
    assert_eq!(r[2]["status"], "not_found");
    assert_eq!(
        r[2]["message"],
        "HMRC has no EORI registration for this number."
    );
    // A UK VAT number without credentials doesn't stop the open EORI check.
    let (code, v) = mock.json(&["check", "GB434031439", "GB2204302310001", "--live"], &[]);
    assert_eq!(code, 3);
    let r = &v["results"];
    assert_eq!(
        (
            r[0]["status"].as_str(),
            r[1]["type"].as_str(),
            r[1]["status"].as_str()
        ),
        (Some("not_configured"), Some("eori"), Some("valid"))
    );
}

#[test]
fn maps_every_hmrc_eori_failure() {
    let cases = [
        (
            reply(
                400,
                json!({"code": "INVALID_REQUEST", "message": "Invalid payload"}).to_string(),
            ),
            "unavailable",
            "HMRC didn't accept the request (Invalid payload). This is not a verdict on the number.",
            Some("INVALID_REQUEST"),
        ),
        (
            Answer::Reply(429, vec![("Retry-After", "60".into())], String::new()),
            "rate_limited",
            "HMRC asked us to slow down (429). Try again in 60 s.",
            Some("429"),
        ),
        (
            reply(200, "[]"),
            "unavailable",
            "HMRC sent a response we couldn't read.",
            None,
        ),
    ];
    for (answer, status, message, code) in cases {
        let answer = std::sync::Mutex::new(Some(answer));
        let mock = Mock::start(move |_| answer.lock().unwrap().take().unwrap_or(reply(503, "")));
        let (exit, v) = mock.json(&["check", "GB220430231000", "-t", "eori", "--live"], &[]);
        let r = first(&v);
        assert_eq!((exit, r["status"].as_str()), (3, Some(status)), "{message}");
        assert_eq!(r["message"], message);
        assert_eq!(r["live"]["code"].as_str(), code);
    }
}
