//! The local page: the HTML itself, and `validate-identifier ui` answering over HTTP on
//! 127.0.0.1.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::{Child, Stdio};

use common::{Mock, command, ended, reply, stderr_of};
use serde_json::{Value, json};

const PAGE: &str = include_str!("../src/ui.html");

#[test]
fn page_reaches_no_other_host() {
    let allowed = [
        "https://nowmyagentcan.com",
        "https://github.com/nowmyagentcan/validate-identifier",
        "https://x.com/chayp05",
        "http://www.w3.org/2000/svg",
    ];
    for (i, _) in PAGE.match_indices("://") {
        let start = PAGE[..i]
            .rfind(|c: char| !c.is_ascii_alphanumeric())
            .map_or(0, |p| p + 1);
        let url = &PAGE[start..];
        assert!(
            allowed.iter().any(|a| url.starts_with(a)),
            "unexpected URL: {}",
            &url[..url.len().min(60)]
        );
    }
    for loader in ["<script src", "<link rel=\"stylesheet\"", "@import", "url("] {
        assert!(!PAGE.contains(loader), "page loads a file: {loader}");
    }
    assert_eq!(PAGE.matches("fetch(").count(), 1);
    assert!(PAGE.contains(
        "fetch(\"/api\", { method: \"POST\", headers: { \"content-type\": \"application/json\" }"
    ));
}

#[test]
fn page_has_the_title_and_one_button() {
    assert!(PAGE.contains("<title>Check VAT, IBAN, phone, email</title>"));
    assert_eq!(PAGE.matches("<button class=\"go\">").count(), 1);
    assert!(PAGE.contains("</svg>Check</button>"));
}

#[test]
fn live_check_starts_off_and_names_every_source() {
    let tag = &PAGE[PAGE.find("id=\"live\"").unwrap()..];
    assert!(!tag[..tag.find('>').unwrap()].contains("checked"));
    let note = &PAGE[PAGE.find("id=\"live-note\"").unwrap()..];
    let note = &note[..note.find("</p>").unwrap()];
    for source in [
        "VIES",
        "HMRC",
        "Swiss UID register",
        "ABN Lookup",
        "GLEIF",
        "EOS",
        "Peppol SML",
        "DNS",
    ] {
        assert!(note.contains(source), "live note misses {source}");
    }
}

/// `validate-identifier ui` on a free port, stopped when dropped.
struct Server {
    child: Child,
    port: u16,
}

impl Server {
    fn start(args: &[&str], env: &[(&str, &str)]) -> Server {
        let mut child = command(&[&["ui"], args].concat(), env)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port = line
            .strip_prefix("Open http://127.0.0.1:")
            .and_then(|rest| rest.strip_suffix("  (Ctrl+C to stop)\n"))
            .unwrap_or_else(|| panic!("{line:?}"))
            .parse()
            .unwrap();
        Server { child, port }
    }

    /// Sends `head` (request line and headers, `{port}` filled in) and `body`; returns the
    /// status, the headers and the body.
    fn send(&self, head: &str, body: &[u8]) -> (u16, String, String) {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        let head = head.replace("{port}", &self.port.to_string());
        stream
            .write_all(format!("{head}\r\n\r\n").as_bytes())
            .unwrap();
        // A server that refused the head may close before the body is all sent.
        let _ = stream.write_all(body);
        let _ = stream.shutdown(Shutdown::Write);
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        let status = headers[9..12].parse().unwrap();
        (status, headers.to_owned(), body.to_owned())
    }

    /// The status of a request with request line `line` and one more header line, raw bytes.
    fn status(&self, line: &[u8], header: &[u8]) -> String {
        let body = r#"{"values":["DE136695976"]}"#;
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        let host = format!(
            "\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
            self.port,
            body.len()
        );
        let request = [line, host.as_bytes(), header, b"\r\n\r\n", body.as_bytes()].concat();
        stream.write_all(&request).unwrap();
        let mut response = Vec::new();
        // A server that drops the request may reset the connection; that reads as no status.
        let _ = stream.read_to_end(&mut response);
        String::from_utf8_lossy(response.get(9..12).unwrap_or_default()).into_owned()
    }

    fn post(&self, request: &Value) -> (u16, Value) {
        let body = request.to_string();
        let head = format!(
            "POST /api HTTP/1.1\r\nHost: 127.0.0.1:{{port}}\r\nOrigin: http://127.0.0.1:{{port}}\r\nContent-Type: application/json\r\nContent-Length: {}",
            body.len()
        );
        let (status, _, body) = self.send(&head, body.as_bytes());
        (status, serde_json::from_str(&body).unwrap())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

/// The CLI's `--json` answer for the same input.
fn cli(args: &[&str]) -> Value {
    let out = command(&[&["check", "--json"], args].concat(), &[])
        .output()
        .unwrap();
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn answers_like_the_cli() {
    let server = Server::start(&["--no-open"], &[]);
    let (status, headers, body) = server.send("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}", b"");
    assert_eq!(status, 200);
    assert_eq!(body, PAGE);
    assert!(headers.contains("Content-Type: text/html; charset=utf-8\r\n"));
    assert!(headers.contains("Content-Security-Policy: default-src 'none';"));
    for (request, args) in [
        (
            json!({"values": ["DE136695976", "+44 20 7946 0958", "GB82WEST1234569876543", "hello"], "type": "auto", "live": false}),
            &[
                "DE136695976",
                "+44 20 7946 0958",
                "GB82WEST1234569876543",
                "hello",
            ][..],
        ),
        (
            json!({"values": ["DE136695976"], "type": "eori"}),
            &["DE136695976", "-t", "eori"],
        ),
        (
            json!({"values": ["07911 123456"], "region": "gb", "requester_vat": "DE136695976"}),
            &["07911 123456", "-r", "gb", "--requester", "DE136695976"],
        ),
    ] {
        assert_eq!(server.post(&request), (200, cli(args)), "{request}");
    }
    for (request, message, hint) in [
        (
            json!({"values": []}),
            "No values to check.",
            "Paste one value per line.",
        ),
        (
            json!({"values": vec!["DE136695976"; 101], "live": true}),
            "Live checks take at most 100 values at a time.",
            "Split the list, or turn off Live check.",
        ),
        (
            json!({"values": ["x"], "type": "isbn"}),
            "Unknown type 'isbn'.",
            "Use one of: auto, iban, vat, phone, email, lei, abn, uid, eori, peppol.",
        ),
        (
            json!({"values": ["x"], "region": "XX"}),
            "Unknown region XX.",
            "Use a 2-letter ISO country code, e.g. GB.",
        ),
    ] {
        let want = json!({"error": {"code": "usage", "message": message, "hint": hint}});
        assert_eq!(server.post(&request), (422, want), "{request}");
    }
    for request in [
        json!({"values": "DE136695976"}),
        json!({"values": ["x"], "lives": true}),
        json!({}),
        json!(["DE136695976"]),
    ] {
        let (status, error) = server.post(&request);
        assert_eq!(
            (status, &error["error"]["code"]),
            (400, &json!("bad_request")),
            "{request}"
        );
    }
}

#[test]
fn checks_live_through_the_overrides_and_keeps_pacing_between_requests() {
    let vies = json!({
        "countryCode": "DE", "vatNumber": "136695976", "requestDate": "2026-10-29T10:12:03.524Z",
        "valid": true, "requestIdentifier": "", "name": "EXAMPLE GMBH", "address": "1 TEST STREET"
    });
    let mock = Mock::start(move |_| reply(200, vies.to_string()));
    let server = Server::start(&["--no-open"], &mock.env());
    let request = json!({"values": ["DE136695976", "GB123456782"], "live": true});
    for _ in 0..2 {
        let (status, answer) = server.post(&request);
        assert_eq!(status, 200, "{answer}");
        let results = &answer["results"];
        assert_eq!(results[0]["live"]["data"]["name"], "EXAMPLE GMBH");
        assert_eq!(results[1]["status"], "not_configured");
    }
    let sent = mock.requests();
    assert_eq!(sent.len(), 2);
    // VIES gets one request a second, from one page request to the next too.
    assert!(sent[1].at - sent[0].at >= std::time::Duration::from_millis(990));
}

#[test]
fn refuses_other_hosts_origins_routes_types_and_large_requests() {
    let server = Server::start(&["--no-open"], &[]);
    let other = server.port.wrapping_add(1);
    for (head, status) in [
        ("GET / HTTP/1.1\r\nHost: localhost:{port}".to_owned(), 200),
        (
            "GET /?from=bookmark HTTP/1.1\r\nHost: localhost:{port}".into(),
            200,
        ),
        ("GET /api?x HTTP/1.1\r\nHost: localhost:{port}".into(), 404),
        ("GET / HTTP/1.1".into(), 403),
        ("GET / HTTP/1.1\r\nHost: evil.example".into(), 403),
        ("GET / HTTP/1.1\r\nHost: evil.example:{port}".into(), 403),
        (format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{other}"), 403),
        (
            "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://evil.example".into(),
            403,
        ),
        (
            "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: https://127.0.0.1:{port}".into(),
            403,
        ),
        (
            "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: null".into(),
            403,
        ),
        (
            "GET /favicon.ico HTTP/1.1\r\nHost: 127.0.0.1:{port}".into(),
            404,
        ),
        (
            "POST / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 0".into(),
            404,
        ),
        ("DELETE /api HTTP/1.1\r\nHost: 127.0.0.1:{port}".into(), 404),
    ] {
        assert_eq!(server.send(&head, b"").0, status, "{head}");
    }
    let (_, _, body) = server.send("GET / HTTP/1.1\r\nHost: evil.example", b"");
    assert_eq!(
        body,
        r#"{"error":{"code":"forbidden","message":"Only this computer's page can use validate-identifier ui.","hint":null}}"#
    );
    let body = r#"{"values":["DE136695976"]}"#;
    for kind in [
        "",
        "\r\nContent-Type: text/plain",
        "\r\nContent-Type: application/x-www-form-urlencoded",
        "\r\nContent-Type: multipart/form-data; boundary=x",
        "\r\nContent-Type: application/jsonx",
    ] {
        let head = format!(
            "POST /api HTTP/1.1\r\nHost: 127.0.0.1:{{port}}{kind}\r\nContent-Length: {}",
            body.len()
        );
        let (status, _, answer) = server.send(&head, body.as_bytes());
        assert_eq!(status, 415, "{kind}");
        assert_eq!(
            answer,
            r#"{"error":{"code":"unsupported_media_type","message":"Send the values as application/json.","hint":null}}"#
        );
    }
    let head = format!(
        "POST /api HTTP/1.1\r\nHost: 127.0.0.1:{{port}}\r\nContent-Type: Application/JSON; charset=utf-8\r\nContent-Length: {}",
        body.len()
    );
    assert_eq!(server.send(&head, body.as_bytes()).0, 200);
    let list = vec!["DE136695976"; 40_000];
    let big = json!({ "values": list }).to_string();
    assert!(big.len() > 512 * 1024);
    let head = format!(
        "POST /api HTTP/1.1\r\nHost: 127.0.0.1:{{port}}\r\nContent-Type: application/json\r\nContent-Length: {}",
        big.len()
    );
    let (status, _, answer) = server.send(&head, big.as_bytes());
    assert_eq!(status, 413);
    assert_eq!(
        answer,
        r#"{"error":{"code":"too_large","message":"The request is larger than 512 KB.","hint":null}}"#
    );
    let head = format!(
        "GET / HTTP/1.1\r\nHost: 127.0.0.1:{{port}}\r\nX-Pad: {}",
        "a".repeat(17 * 1024)
    );
    assert_eq!(server.send(&head, b"").0, 413);
}

#[test]
fn refuses_repeated_headers_and_short_bodies() {
    let server = Server::start(&["--no-open"], &[]);
    let body = r#"{"values":["DE136695976"]}"#;
    let post = |headers: &str, length: usize| {
        let head = format!(
            "POST /api HTTP/1.1\r\n{headers}\r\nContent-Type: application/json\r\nContent-Length: {length}"
        );
        server.send(&head, body.as_bytes()).0
    };
    let local = "Host: 127.0.0.1:{port}";
    assert_eq!(post(local, body.len()), 200);
    for headers in [
        "Host: evil.example\r\nHost: 127.0.0.1:{port}",
        "Host: 127.0.0.1:{port}\r\nHost: 127.0.0.1:{port}",
        "Host: 127.0.0.1:{port}\r\nOrigin: http://evil.example\r\nOrigin: http://127.0.0.1:{port}",
        "Host: 127.0.0.1:{port}\r\nContent-Length: 0",
        "Host: 127.0.0.1:{port}\r\nContent-Type: text/plain",
    ] {
        assert_eq!(post(headers, body.len()), 400, "{headers}");
    }
    assert_eq!(post(local, body.len() + 10), 400);
    let hidden =
        "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nNoColon\r\nOrigin: http://evil.example";
    assert_eq!(server.send(hidden, b"").0, 400);
}

#[test]
fn refuses_malformed_headers_framing_and_request_lines() {
    let server = Server::start(&["--no-open"], &[]);
    let body = r#"{"values":["DE136695976"]}"#;
    let length = format!(
        "Content-Type: application/json\r\nContent-Length: {}",
        body.len()
    );
    let head = format!("POST /api HTTP/1.1\r\nHost: 127.0.0.1:{{port}}\r\n{length}");
    for head in [
        format!("{head}\r\nOrigin : http://evil.example"),
        format!("{head}\r\nBad Header: value"),
        format!("{head}\r\nTransfer-Encoding: chunked"),
        format!("{head}\r\nTransfer-Encoding: identity"),
        head.replace("Length: ", "Length: +"),
        head.replace("Length: ", "Length: -"),
        head.replace(" HTTP/1.1", ""),
        head.replace("HTTP/1.1", "XYZ"),
        head.replace("POST /api", "POST  /api"),
    ] {
        assert_eq!(server.send(&head, body.as_bytes()).0, 400, "{head}");
    }
    let status = |line: &[u8], header: &[u8]| server.status(line, header);
    let post = b"POST /api HTTP/1.1".as_slice();
    // RFC 9110 §5.5: field values may carry opaque bytes 80-FF; an unknown field is ignored.
    for byte in [0x80, 0xe9, 0xff] {
        assert_eq!(status(post, &[b"X-Other: ", &[byte][..]].concat()), "200");
    }
    assert_eq!(status(post, b"\xe9: x"), "400");
    // Control bytes are neither field content nor part of a target (RFC 9110 §5.5, RFC 9112 §3).
    for header in [
        &b"X-Other: a\0b"[..],
        b"X-Other: a\rb",
        b"X-Other: \x0ca",
        b"X-Other: a\x0c",
    ] {
        assert_eq!(status(post, header), "400", "{header:?}");
    }
    assert_eq!(status(post, b"X-Other: \ta\t"), "200");
    assert_eq!(status(b"POST /api\tx HTTP/1.1", b"X-Other: a"), "400");
    assert_eq!(status(b"POST /api\xe9 HTTP/1.1", b"X-Other: a"), "400");
    let raw = b"{\"values\":[\"\xff\"]}";
    let head = format!(
        "POST /api HTTP/1.1\r\nHost: 127.0.0.1:{{port}}\r\nContent-Type: application/json\r\nContent-Length: {}",
        raw.len()
    );
    assert_eq!(server.send(&head, raw).0, 400);
}

#[test]
fn reports_a_busy_port() {
    let taken = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = taken.local_addr().unwrap().port().to_string();
    let out = command(&["ui", "--no-open", "--port", &port], &[])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        format!("✗ Port {port} is in use.\n  Use --port 0 for a free port.\n")
    );
}

/// A stand-in for xdg-open and open that writes down the address it was given.
#[cfg(unix)]
#[test]
fn opens_the_browser_unless_told_not_to() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!(
        "validate-identifier-browser-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let opened = dir.join("opened");
    for opener in ["xdg-open", "open"] {
        let script = dir.join(opener);
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho \"$1\" > '{}'\n", opened.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap());
    let opened_url = || std::fs::read_to_string(&opened).ok();
    let start = |args: &[&str]| {
        let server = Server::start(args, &[("PATH", &path)]);
        std::thread::sleep(std::time::Duration::from_millis(500));
        server.port
    };
    start(&["--no-open"]);
    assert_eq!(opened_url(), None);
    let port = start(&[]);
    assert_eq!(opened_url(), Some(format!("http://127.0.0.1:{port}\n")));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ends_quietly_when_stdout_closes() {
    // The read end closes before the spawn, so no test spawning beside this one holds it.
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let mut child = command(&["ui", "--no-open"], &[])
        .stdout(writer)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert_eq!(
        (ended(&mut child), stderr_of(&mut child)),
        (Some(0), String::new())
    );
}

#[test]
fn keeps_serving_when_clients_hang_up() {
    let vies = json!({
        "countryCode": "DE", "vatNumber": "136695976", "requestDate": "2026-10-29T10:12:03.524Z",
        "valid": true, "requestIdentifier": "", "name": "EXAMPLE GMBH", "address": "1 TEST STREET"
    });
    // A registry that takes its time, so the page's client is gone before the answer.
    let mock = Mock::start(move |_| {
        std::thread::sleep(std::time::Duration::from_millis(500));
        reply(200, vies.to_string())
    });
    let mut child = command(&["ui", "--no-open"], &mock.env())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port: u16 = line
        .trim_end()
        .trim_end_matches("  (Ctrl+C to stop)")
        .rsplit(':')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let post = |body: &str| {
        format!(
            "POST /api HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    };
    let offline = post(r#"{"values":["DE44500105175407324931"]}"#);
    let live = post(r#"{"values":["DE136695976"],"live":true}"#);
    let connect = || TcpStream::connect(("127.0.0.1", port)).unwrap();
    // Leaves after half the headers, after half the body, before reading the answer, and
    // while a live check is still running.
    connect().write_all(&offline.as_bytes()[..20]).unwrap();
    connect()
        .write_all(&offline.as_bytes()[..offline.len() - 5])
        .unwrap();
    for request in [&offline, &live] {
        for _ in 0..10 {
            let mut s = connect();
            s.write_all(request.as_bytes()).unwrap();
            s.shutdown(Shutdown::Both).unwrap();
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let mut s = connect();
    s.write_all(offline.as_bytes()).unwrap();
    let mut answer = String::new();
    s.read_to_string(&mut answer).unwrap();
    assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    assert!(answer.contains(r#""status":"valid""#));
    assert_eq!(child.try_wait().unwrap(), None, "the server stopped");
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(stderr_of(&mut child), "");
}

#[test]
fn runs_with_stderr_closed() {
    let mut child = command(&["ui", "--no-open"], &[])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stderr.take());
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(line.starts_with("Open http://127.0.0.1:"), "{line}");
    let port = line.trim_end().trim_end_matches("  (Ctrl+C to stop)");
    let port = port.rsplit(':').next().unwrap();
    // The port stays taken by the first server while a second one asks for it.
    let mut busy = command(&["ui", "--no-open", "--port", port], &[])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(busy.stderr.take());
    assert_eq!(ended(&mut busy), Some(3));
    child.kill().unwrap();
    child.wait().unwrap();
    let out = command(&["mcp"], &[])
        .stderr(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
}
