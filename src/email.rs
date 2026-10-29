use std::net::{Ipv4Addr, Ipv6Addr};

use serde_json::json;

use crate::item::{Error, Found, Warning, checks};

const CHECKS: [&str; 3] = ["syntax", "domain", "length"];

/// Checks one address (RFC 5321/5322 with RFC 6531/6532 internationalised parts, UTS 46 domains).
pub fn check(v: &str) -> Found {
    match parse(v) {
        Ok((local, domain, ascii, warnings)) => {
            let message = match domain.starts_with('[') {
                true => "Syntax passes.",
                false => "Syntax passes. Mail servers not checked; add --live to look them up.",
            };
            Found {
                value: Some(format!("{local}@{ascii}")),
                country: None,
                checks: checks(&CHECKS, None),
                details: json!({"local": local, "domain": domain, "ascii_domain": ascii, "smtputf8": !local.is_ascii()}),
                outcome: Ok(message.into()),
                warnings,
            }
        }
        Err((i, e)) => Found {
            checks: checks(&CHECKS, Some(i)),
            ..Found::error(e)
        },
    }
}

type Parsed<'a> = (&'a str, &'a str, String, Vec<Warning>);

fn parse(v: &str) -> Result<Parsed<'_>, (usize, Error)> {
    let syntax = |code, message: &str, hint| Err((0, Error::new(code, message, hint)));
    let at = || syntax("email_at", "An email address has exactly one @.", None);
    let bad_local = || {
        let message = "The part before @ isn't valid (dot at the start or end, two dots, or a forbidden character).";
        syntax("email_local", message, None)
    };
    // A quoted local part may hold any character; a display name is outside it.
    let unquoted = match v.starts_with('"') {
        true => quoted_end(v).map_or(v, |end| &v[end + 1..]),
        false => v,
    };
    if unquoted.contains(['<', '>']) {
        return syntax(
            "email_display_name",
            "Pass only the address, without a name.",
            Some("e.g. jane@example.com"),
        );
    }
    let mut warnings = Vec::new();
    let (local, domain) = if v.starts_with('"') {
        let Some(end) = quoted_end(v) else {
            return bad_local();
        };
        let (local, rest) = v.split_at(end + 1);
        match rest.strip_prefix('@') {
            Some(domain) if !domain.contains('@') => (local, domain),
            _ => return at(),
        }
    } else {
        let Some((local, domain)) = v.split_once('@') else {
            return at();
        };
        if domain.contains('@') {
            return at();
        }
        let atext = |c: char| {
            !c.is_ascii() || c.is_ascii_alphanumeric() || "!#$%&'*+-/=?^_`{|}~".contains(c)
        };
        if local
            .split('.')
            .any(|atom| atom.is_empty() || !atom.chars().all(atext))
        {
            return bad_local();
        }
        (local, domain)
    };
    if local.starts_with('"') {
        warnings.push(Warning {
            code: "quoted_local_part",
            message: "Quoted local parts are valid but many mail systems reject them.".into(),
        });
    }

    let ascii = if let Some(literal) = domain.strip_prefix('[').and_then(|d| d.strip_suffix(']')) {
        let ip = match literal.get(..5) {
            Some(tag) if tag.eq_ignore_ascii_case("IPv6:") => {
                literal[5..].parse::<Ipv6Addr>().is_ok()
            }
            _ => literal.parse::<Ipv4Addr>().is_ok(),
        };
        if !ip {
            return Err(bad_domain("the address in brackets isn't an IP address"));
        }
        warnings.push(Warning {
            code: "address_literal",
            message: "The domain is an IP address; mail server lookup skipped.".into(),
        });
        domain.to_string()
    } else {
        ascii_domain(domain).map_err(bad_domain)?
    };

    let length = |message: &str| Err((2, Error::new("email_length", message, None)));
    if local.len() > 64 {
        return length("The part before @ is longer than 64 bytes.");
    }
    if local.len() + 1 + ascii.len() > 254 {
        return length("The address is longer than 254 bytes.");
    }
    Ok((local, domain, ascii, warnings))
}

/// The index of the quote that closes the quoted local part opening `v`.
fn quoted_end(v: &str) -> Option<usize> {
    let mut chars = v.char_indices().skip(1);
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some(i),
            '\\' => {
                chars.next().filter(|&(_, c)| !c.is_control())?;
            }
            c if c.is_control() => return None,
            _ => {}
        }
    }
    None
}

fn ascii_domain(domain: &str) -> Result<String, &'static str> {
    if domain.is_empty() {
        return Err("it is empty");
    }
    if domain.ends_with('.') {
        return Err("it ends with a dot");
    }
    let forbidden = "it has characters a domain name can't have";
    let ascii = idna::domain_to_ascii(domain).map_err(|_| forbidden)?;
    let labels: Vec<_> = ascii.split('.').collect();
    if labels.len() < 2 {
        return Err("it needs a dot, as in example.com");
    }
    for label in &labels {
        if label.is_empty() {
            return Err("it has two dots in a row");
        }
        if label.len() > 63 {
            return Err("a part between dots is longer than 63 characters");
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(forbidden);
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err("a part between dots starts or ends with a hyphen");
        }
    }
    if labels
        .last()
        .is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err("the last part is all digits");
    }
    Ok(ascii)
}

fn bad_domain(reason: &str) -> (usize, Error) {
    let message = format!("The domain isn't valid: {reason}.");
    (
        1,
        Error::new(
            "email_domain",
            message,
            Some("Check the spelling after the @."),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(found: &Found) -> Vec<&str> {
        found.warnings.iter().map(|w| w.code).collect()
    }

    #[test]
    fn valid_addresses() {
        let found = check("Jane.O'Neil+tag@Example.COM");
        assert_eq!(found.value.as_deref(), Some("Jane.O'Neil+tag@example.com"));
        assert_eq!(
            found.outcome,
            Ok("Syntax passes. Mail servers not checked; add --live to look them up.".into())
        );
        assert_eq!(found.checks.len(), 3);
        for v in [
            "a@b.co",
            "x!#$%&'*+-/=?^_`{|}~@sub-1.example.org",
            "a@xn--bcher-kva.de",
            "a@1.2.3.example",
        ] {
            assert!(check(v).outcome.is_ok(), "{v}");
        }
    }

    #[test]
    fn internationalised_addresses_give_their_ascii_domain() {
        let found = check("user@bücher.de");
        assert_eq!(found.value.as_deref(), Some("user@xn--bcher-kva.de"));
        assert_eq!(
            found.details,
            json!({"local": "user", "domain": "bücher.de", "ascii_domain": "xn--bcher-kva.de", "smtputf8": false})
        );
        let found = check("josé@BÜCHER.de");
        assert_eq!(found.details["ascii_domain"], "xn--bcher-kva.de");
        assert_eq!(found.details["smtputf8"], true);
        assert_eq!(
            check("用户@例子.广告").details["ascii_domain"],
            "xn--fsqu00a.xn--4rr70v"
        );
    }

    #[test]
    fn quoted_local_parts_and_address_literals_pass_with_a_warning() {
        for v in [
            r#""john doe"@example.com"#,
            r#""a\"b@c"@example.com"#,
            r#""a.@..b"@example.com"#,
        ] {
            let found = check(v);
            assert!(found.outcome.is_ok(), "{v}");
            assert_eq!(codes(&found), ["quoted_local_part"], "{v}");
        }
        for v in [
            "jane@[192.0.2.1]",
            "jane@[IPv6:2001:db8::1]",
            "jane@[ipv6:::1]",
        ] {
            let found = check(v);
            assert_eq!(found.outcome, Ok("Syntax passes.".into()), "{v}");
            assert_eq!(codes(&found), ["address_literal"]);
            assert_eq!(found.details["ascii_domain"], &v[5..]);
        }
    }

    #[test]
    fn each_failure_gives_its_error() {
        let local = "The part before @ isn't valid (dot at the start or end, two dots, or a forbidden character).";
        let long_label = format!("a@{}.com", "x".repeat(64));
        let long_local = format!("{}@example.com", "x".repeat(65));
        let long_address = format!(
            "{}@{}.com",
            "x".repeat(64),
            vec!["y".repeat(63); 3].join(".")
        );
        let cases = [
            (
                "Jane Doe <jane@example.com>",
                "email_display_name",
                "Pass only the address, without a name.",
            ),
            (
                "jane.example.com",
                "email_at",
                "An email address has exactly one @.",
            ),
            (
                "jane@doe@example.com",
                "email_at",
                "An email address has exactly one @.",
            ),
            (
                r#""jane"doe@example.com"#,
                "email_at",
                "An email address has exactly one @.",
            ),
            ("@example.com", "email_local", local),
            (".jane@example.com", "email_local", local),
            ("jane.@example.com", "email_local", local),
            ("ja..ne@example.com", "email_local", local),
            ("ja ne@example.com", "email_local", local),
            ("ja(ne)@example.com", "email_local", local),
            (r#""jane@example.com"#, "email_local", local),
            (
                "jane@",
                "email_domain",
                "The domain isn't valid: it is empty.",
            ),
            (
                "jane@example",
                "email_domain",
                "The domain isn't valid: it needs a dot, as in example.com.",
            ),
            (
                "jane@example.com.",
                "email_domain",
                "The domain isn't valid: it ends with a dot.",
            ),
            (
                "jane@example..com",
                "email_domain",
                "The domain isn't valid: it has two dots in a row.",
            ),
            (
                "jane@exa_mple.com",
                "email_domain",
                "The domain isn't valid: it has characters a domain name can't have.",
            ),
            (
                "jane@xn--a.com",
                "email_domain",
                "The domain isn't valid: it has characters a domain name can't have.",
            ),
            (
                "jane@-example.com",
                "email_domain",
                "The domain isn't valid: a part between dots starts or ends with a hyphen.",
            ),
            (
                "jane@1.2.3.4",
                "email_domain",
                "The domain isn't valid: the last part is all digits.",
            ),
            (
                "jane@[1.2.3]",
                "email_domain",
                "The domain isn't valid: the address in brackets isn't an IP address.",
            ),
            (
                &long_label,
                "email_domain",
                "The domain isn't valid: a part between dots is longer than 63 characters.",
            ),
            (
                &long_local,
                "email_length",
                "The part before @ is longer than 64 bytes.",
            ),
            (
                &long_address,
                "email_length",
                "The address is longer than 254 bytes.",
            ),
        ];
        for (v, code, message) in cases {
            let found = check(v);
            let e = found.outcome.expect_err(v);
            assert_eq!((e.code, e.message.as_str()), (code, message), "{v}");
            assert_eq!(found.checks.last().unwrap().pass, Some(false), "{v}");
            assert_eq!(found.value, None);
        }
    }
}
