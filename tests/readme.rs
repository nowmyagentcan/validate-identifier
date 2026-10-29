//! Every offline command in README.md runs, and the pasted output matches the binary's.

mod common;

use std::path::Path;

const README: &str = include_str!("../README.md");

/// Splits an example the way a POSIX shell does here: spaces, double quotes, `  # comment`.
fn words(line: &str) -> Vec<String> {
    let line = line.split("  #").next().unwrap();
    let mut out = Vec::new();
    let (mut word, mut quoted) = (None::<String>, false);
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                word.get_or_insert_default();
            }
            ' ' if !quoted => out.extend(word.take()),
            _ => word.get_or_insert_default().push(c),
        }
    }
    out.extend(word);
    out
}

/// Exit code and stdout of the example, run in `dir`.
fn run(line: &str, dir: &Path) -> (i32, String) {
    let words = words(line);
    assert_eq!(words[0], "validate-identifier", "{line}");
    let args: Vec<&str> = words[1..].iter().map(String::as_str).collect();
    let out = common::command(&args, &[])
        .current_dir(dir)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.is_empty(), "{line}: {stderr}");
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
    )
}

#[test]
fn every_offline_example_runs_as_shown() {
    let dir = std::env::temp_dir().join(format!("vi-readme-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("ibans.txt"),
        "DE44500105175407324931\nGB82 WEST 1234 5698 7654 32\n",
    )
    .unwrap();
    let (mut commands, mut outputs) = (0, 0);
    for block in README.split("```").skip(1).step_by(2) {
        let (lang, body) = block.split_once('\n').unwrap();
        if lang == "text" {
            let (line, expected) = body.split_once('\n').unwrap();
            let (code, out) = run(line.strip_prefix("$ ").unwrap(), &dir);
            assert_eq!(out, expected, "{line}");
            assert_eq!(code, i32::from(expected.contains('✗')), "{line}");
            outputs += 1;
        } else if lang == "sh" {
            let offline = |l: &&str| l.starts_with("validate-identifier ") && !l.contains("--live");
            for line in body.lines().filter(offline) {
                let (code, out) = run(line, &dir);
                assert_eq!(code, 0, "{line}");
                if line.contains("--json") {
                    serde_json::from_str::<serde_json::Value>(&out).expect(line);
                }
                commands += 1;
            }
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!((commands, outputs), (6, 1));
}
