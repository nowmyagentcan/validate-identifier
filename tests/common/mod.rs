//! A loopback HTTP server and a UDP DNS server standing in for the registries, and the binary
//! pointed at them. No test reaches the real services: every source's base URL is overridden
//! and DNS goes to the stub through `--dns`.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use hickory_resolver::proto::op::{Message, ResponseCode};
use hickory_resolver::proto::rr::{Name, RData, Record, RecordType};
use serde_json::Value;

/// The base URL override of every HTTP source.
pub const SOURCES: [&str; 6] = [
    "VALIDATE_IDENTIFIER_VIES_URL",
    "VALIDATE_IDENTIFIER_EOS_URL",
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
    /// When the mock began serving it: a busy machine may start one later than another,
    /// so gaps between two are checked with 50 ms of slack.
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

    /// Every source's base URL override, pointing here.
    pub fn env(&self) -> Vec<(&'static str, &str)> {
        SOURCES.iter().map(|&v| (v, self.url.as_str())).collect()
    }

    pub fn requests(&self) -> Vec<Request> {
        self.log.lock().unwrap().clone()
    }

    /// Runs the binary with every source pointed here; exit code, stdout, stderr.
    pub fn run(&self, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
        let mut all = self.env();
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

/// The binary without any proxy, credential or source override from this machine.
pub fn command(args: &[&str], env: &[(&str, &str)]) -> Command {
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
    cmd
}

/// Runs the binary as `command` sets it up; exit code, stdout, stderr.
pub fn vi(args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let out = command(args, env).output().unwrap();
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

/// What the DNS stub answers to one query.
pub enum Dns {
    Records(Vec<RData>),
    NxDomain,
    ServFail,
    /// Never answers.
    Silence,
}

/// A UDP DNS server on loopback for `--dns`; it records each query's name and type.
pub struct DnsStub {
    pub addr: String,
    log: Arc<Mutex<Vec<(String, RecordType)>>>,
}

impl DnsStub {
    pub fn start(answer: impl Fn(&str, RecordType) -> Dns + Send + 'static) -> DnsStub {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = socket.local_addr().unwrap().to_string();
        let log = Arc::new(Mutex::new(Vec::new()));
        let queries = Arc::clone(&log);
        thread::spawn(move || {
            let mut buf = [0; 4096];
            while let Ok((n, from)) = socket.recv_from(&mut buf) {
                let Ok(asked) = Message::from_vec(&buf[..n]) else {
                    continue;
                };
                let query = asked.queries[0].clone();
                let name = query
                    .name
                    .to_ascii()
                    .trim_end_matches('.')
                    .to_ascii_lowercase();
                queries
                    .lock()
                    .unwrap()
                    .push((name.clone(), query.query_type));
                let mut m = Message::response(asked.metadata.id, asked.metadata.op_code);
                m.metadata.recursion_desired = asked.metadata.recursion_desired;
                m.metadata.recursion_available = true;
                m.add_query(query.clone());
                match answer(&name, query.query_type) {
                    Dns::Records(data) => {
                        for d in data {
                            m.add_answer(Record::from_rdata(query.name.clone(), 60, d));
                        }
                    }
                    Dns::NxDomain => m.metadata.response_code = ResponseCode::NXDomain,
                    Dns::ServFail => m.metadata.response_code = ResponseCode::ServFail,
                    Dns::Silence => continue,
                }
                let _ = socket.send_to(&m.to_vec().unwrap(), from);
            }
        });
        DnsStub { addr, log }
    }

    pub fn queries(&self) -> Vec<(String, RecordType)> {
        self.log.lock().unwrap().clone()
    }
}

/// A DNS name from text.
pub fn name(s: &str) -> Name {
    Name::from_ascii(s).unwrap()
}

/// A loopback UDP address where nothing listens, for `--dns`.
pub fn closed_udp_port() -> String {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string()
}

/// The exit code once `child` ends, waiting up to 10 s; `None` (after killing it) if it
/// doesn't.
pub fn ended(child: &mut std::process::Child) -> Option<i32> {
    for _ in 0..500 {
        if let Some(status) = child.try_wait().unwrap() {
            return status.code();
        }
        thread::sleep(Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    None
}

/// What `child` wrote to stderr.
pub fn stderr_of(child: &mut std::process::Child) -> String {
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    err
}
