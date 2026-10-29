//! A loopback HTTP server standing in for the registries, and the binary pointed at it. No
//! test reaches the real services: every source's base URL is overridden.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

/// The base URL override of every HTTP source.
pub const SOURCES: [&str; 5] = [
    "VALIDATE_IDENTIFIER_VIES_URL",
    "VALIDATE_IDENTIFIER_HMRC_URL",
    "VALIDATE_IDENTIFIER_UID_URL",
    "VALIDATE_IDENTIFIER_ABN_URL",
    "VALIDATE_IDENTIFIER_GLEIF_URL",
];

/// Test credentials; no run against the mock may print them.
pub const HMRC_CREDS: [(&str, &str); 2] = [
    ("HMRC_CLIENT_ID", "id-abc123"),
    ("HMRC_CLIENT_SECRET", "s3cret/xyz"),
];
pub const GUID: (&str, &str) = ("ABN_LOOKUP_GUID", "f0e1d2c3-b4a5-9687-7869-5a4b3c2d1e0f");
pub const TOKEN: &str = "tok-9f8e7d";

/// One request as the mock received it.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    /// Path and query.
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub at: Instant,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        let mut found = self.headers.iter().filter(|(n, _)| n == name);
        found.next().map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap()
    }
}

pub enum Answer {
    Reply(u16, Vec<(&'static str, String)>, String),
    /// Keeps the connection open without answering.
    Hang,
    /// Closes the connection without answering.
    Close,
}

pub fn reply(status: u16, body: impl Into<String>) -> Answer {
    Answer::Reply(status, Vec::new(), body.into())
}

pub struct Mock {
    pub url: String,
    log: Arc<Mutex<Vec<Request>>>,
}

impl Mock {
    pub fn start(answer: impl Fn(&Request) -> Answer + Send + 'static) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let log = Arc::new(Mutex::new(Vec::new()));
        let requests = Arc::clone(&log);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = serve(stream, &answer, &requests);
            }
        });
        Mock { url, log }
    }

    pub fn requests(&self) -> Vec<Request> {
        self.log.lock().unwrap().clone()
    }

    /// Runs the binary with every source pointed here; exit code, stdout, stderr.
    pub fn run(&self, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
        let mut all: Vec<(&str, &str)> = SOURCES.iter().map(|&v| (v, self.url.as_str())).collect();
        all.extend_from_slice(env);
        let (code, out, err) = vi(args, &all);
        for secret in [
            HMRC_CREDS[0].1,
            HMRC_CREDS[1].1,
            "s3cret%2Fxyz",
            GUID.1,
            TOKEN,
        ] {
            assert!(
                !out.contains(secret) && !err.contains(secret),
                "{secret}: {out}{err}"
            );
        }
        (code, out, err)
    }

    /// The `--json` answer of a run against the mock.
    pub fn json(&self, args: &[&str], env: &[(&str, &str)]) -> (i32, Value) {
        let (code, out, err) = self.run(&[args, &["--json"]].concat(), env);
        assert_eq!(err, "", "{args:?}");
        (code, serde_json::from_str(&out).unwrap())
    }
}

/// Answers one request; a client that hangs up early ends it.
fn serve(
    mut stream: TcpStream,
    answer: &impl Fn(&Request) -> Answer,
    log: &Mutex<Vec<Request>>,
) -> std::io::Result<()> {
    let at = Instant::now();
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace().map(String::from);
    let (method, target) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    let mut headers = Vec::new();
    loop {
        line.clear();
        reader.read_line(&mut line)?;
        let Some((name, value)) = line.trim_end().split_once(':') else {
            break;
        };
        headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
    }
    let length = headers.iter().find(|(n, _)| n == "content-length");
    let mut body = vec![0; length.map_or(0, |(_, v)| v.parse().unwrap())];
    reader.read_exact(&mut body)?;
    let request = Request {
        method,
        target,
        headers,
        body: String::from_utf8(body).unwrap(),
        at,
    };
    let answer = answer(&request);
    log.lock().unwrap().push(request);
    match answer {
        Answer::Reply(status, headers, body) => {
            let mut head = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for (name, value) in headers {
                head += &format!("{name}: {value}\r\n");
            }
            let _ = stream.write_all(format!("{head}\r\n{body}").as_bytes());
        }
        Answer::Hang => {
            thread::spawn(move || {
                thread::sleep(Duration::from_secs(60));
                drop(stream);
            });
        }
        Answer::Close => {}
    }
    Ok(())
}

/// Runs the binary without any proxy, credential or source override from this machine.
pub fn vi(args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_validate-identifier"));
    cmd.args(args).stdin(Stdio::null());
    for (name, _) in std::env::vars() {
        let upper = name.to_ascii_uppercase();
        if upper.ends_with("_PROXY")
            || upper.starts_with("VALIDATE_IDENTIFIER_")
            || upper.starts_with("HMRC_")
            || upper == "ABN_LOOKUP_GUID"
        {
            cmd.env_remove(name);
        }
    }
    cmd.env("NO_COLOR", "1").envs(env.iter().copied());
    let out = cmd.output().unwrap();
    let text = |b: Vec<u8>| String::from_utf8(b).unwrap();
    (
        out.status.code().unwrap(),
        text(out.stdout),
        text(out.stderr),
    )
}

/// A loopback URL where nothing listens.
pub fn closed_port() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}", listener.local_addr().unwrap())
}
