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

/// The request start times `--debug` printed for `source`, in seconds since the first.
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
    assert!(at.windows(2).all(|w| w[1] - w[0] >= 1.0), "{at:?}");
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

const HMRC_CREDS: [(&str, &str); 2] = [
    ("HMRC_CLIENT_ID", "id-abc123"),
    ("HMRC_CLIENT_SECRET", "s3cret/xyz"),
];

/// A mock HMRC that hands out a token, then answers lookups with `lookup(path)`.
fn hmrc(lookup: impl Fn(&str) -> Answer + Send + 'static) -> Mock {
    Mock::start(move |r| {
        match r.target.as_str() {
        "/oauth/token" => reply(
            200,
            json!({"access_token": "tok-9f8e7d", "token_type": "bearer", "expires_in": 14400, "scope": "read:vat"})
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
        assert_eq!(lookup.header("authorization"), Some("Bearer tok-9f8e7d"));
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
fn never_prints_hmrc_secrets_or_tokens() {
    let mock = hmrc(|_| hmrc_ok());
    for args in [&["--json"][..], &[], &["--debug"]] {
        let (code, out, err) = mock.run(
            &[&["check", "GB434031439", "--live"][..], args].concat(),
            &HMRC_CREDS,
        );
        assert_eq!(code, 0);
        for secret in ["s3cret", "tok-9f8e7d", "id-abc123"] {
            assert!(
                !out.contains(secret) && !err.contains(secret),
                "{secret} in {out}{err}"
            );
        }
    }
}
