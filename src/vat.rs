//! VAT number formats (the VIES format table) and check digits.
//!
//! The check digit functions are ported from valvat (https://github.com/yolk/valvat),
//! commit 5802c029, lib/valvat/checksum/<country>.rb, under its licence:
//!
//! Copyright (c) mite GmbH
//!
//! Permission is hereby granted, free of charge, to any person obtaining
//! a copy of this software and associated documentation files (the
//! "Software"), to deal in the Software without restriction, including
//! without limitation the rights to use, copy, modify, merge, publish,
//! distribute, sublicense, and/or sell copies of the Software, and to
//! permit persons to whom the Software is furnished to do so, subject to
//! the following conditions:
//!
//! The above copyright notice and this permission notice shall be
//! included in all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
//! EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
//! MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
//! NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE
//! LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
//! OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION
//! WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

use serde_json::json;

use crate::item::{Check, Error, Found, a, checks};

/// The pattern of the part after the 2-letter code and its words for messages.
/// Pattern letters: 9 a digit, N 1-9, C 0-6 or 9, A a capital letter,
/// X a capital letter but I or O, or a digit; anything else stands for itself; | separates choices.
fn format(code: &str) -> Option<(&'static str, &'static str)> {
    Some(match code {
        "AT" => ("U99999999", "U and 8 digits"),
        "BE" => ("0999999999|1999999999", "10 digits starting with 0 or 1"),
        "BG" => ("999999999|9999999999", "9 or 10 digits"),
        "CY" => ("C9999999A", "8 digits and a letter"),
        "CZ" => ("99999999|999999999|9999999999", "8, 9 or 10 digits"),
        "DE" => ("999999999", "9 digits"),
        "DK" => ("99999999", "8 digits"),
        "EE" => ("109999999", "9 digits starting with 10"),
        "EL" => ("999999999", "9 digits"),
        "ES" => (
            "A99999999|99999999A|A9999999A",
            "9 characters: a letter or digit, 7 digits, a letter or digit",
        ),
        "FI" => ("99999999", "8 digits"),
        "FR" => ("XX999999999", "2 letters or digits and 9 digits"),
        "HR" => ("99999999999", "11 digits"),
        "HU" => ("99999999", "8 digits"),
        "IE" => (
            "9A99999A|9999999A|9999999AA",
            "8 or 9 characters: 7 digits and 1 or 2 letters",
        ),
        "IT" => ("99999999999", "11 digits"),
        "LT" => ("999999919|999999999919", "9 or 12 digits"),
        "LU" => ("99999999", "8 digits"),
        "LV" => ("99999999999", "11 digits"),
        "MT" => ("99999999", "8 digits"),
        "NL" => ("999999999B99", "9 digits, B and 2 digits"),
        "PL" => ("9999999999", "10 digits"),
        "PT" => ("999999999", "9 digits"),
        "RO" => (
            "N9|N99|N999|N9999|N99999|N999999|N9999999|N99999999|N999999999",
            "2 to 10 digits",
        ),
        "SE" => ("999999999901", "12 digits ending in 01"),
        "SI" => ("99999999", "8 digits"),
        "SK" => ("9999999999", "10 digits"),
        "GB" | "XI" => (
            "999999999|999999999999|GD999|HA999",
            "9 or 12 digits, or GD or HA and 3 digits",
        ),
        "CH" => (
            "E999999999MWST|E999999999TVA|E999999999IVA|E999999999TPV",
            "9 digits and MWST, TVA, IVA or TPV",
        ),
        _ => return None,
    })
}

fn matches(pattern: &str, s: &str) -> bool {
    let class = |p: u8, c: u8| match p {
        b'9' => c.is_ascii_digit(),
        b'N' => matches!(c, b'1'..=b'9'),
        b'C' => matches!(c, b'0'..=b'6' | b'9'),
        b'A' => c.is_ascii_uppercase(),
        b'X' => c.is_ascii_digit() || (c.is_ascii_uppercase() && c != b'I' && c != b'O'),
        _ => p == c,
    };
    pattern
        .split('|')
        .any(|alt| alt.len() == s.len() && alt.bytes().zip(s.bytes()).all(|(p, c)| class(p, c)))
}

const CHECKS: [&str; 3] = ["country", "format", "check_digits"];

/// Checks a normalised VAT number (compact, `GR` already rewritten to `EL`).
pub fn check(v: &str) -> Found {
    let fail = |i, e| Found {
        value: Some(v.to_string()),
        checks: checks(&CHECKS, Some(i)),
        ..Found::error(e)
    };
    let code = v
        .get(..2)
        .filter(|c| c.bytes().all(|b| b.is_ascii_uppercase()));
    let Some(code) = code else {
        let e = Error::new(
            "vat_prefix",
            "A VAT number needs its country prefix.",
            Some("Write it as e.g. DE136695976."),
        );
        return fail(0, e);
    };
    let Some((pattern, words)) = format(code) else {
        let hint = Some("Supported: EU countries (EL for Greece), XI, GB and CH.");
        return fail(
            0,
            Error::new("vat_country", format!("No VAT rules for {code}."), hint),
        );
    };
    let rest = &v[2..];
    let prefix = if code == "CH" { "CHE" } else { code };
    let country = match code {
        "EL" => "GR",
        c => c,
    };
    if !matches(pattern, rest) || (code == "CY" && rest.starts_with("12")) {
        let message = format!("{prefix} VAT numbers are {prefix} followed by {words}.");
        let e = Error::new(
            "vat_format",
            message,
            Some("Check the number on the invoice or registration."),
        );
        return Found {
            country: Some(country.into()),
            ..fail(1, e)
        };
    }

    let source = match code {
        "GB" => "HMRC",
        "CH" => "the UID register",
        _ => "VIES",
    };
    let (number, branch, value) = match code {
        "CH" => {
            let (d, suffix) = rest[1..].split_at(9);
            (
                d,
                None,
                format!("CHE-{}.{}.{} {suffix}", &d[..3], &d[3..6], &d[6..]),
            )
        }
        "GB" | "XI" if rest.len() == 12 => (&rest[..9], Some(&rest[9..]), v.to_string()),
        _ => (rest, None, v.to_string()),
    };
    let digits = check_digits(code, rest);
    let mut checks = checks(&CHECKS[..2], None);
    checks.push(Check {
        name: "check_digits",
        pass: digits,
    });
    let unchecked = format!("Not checked against {source}; add --live to confirm registration.");
    let outcome = match digits {
        Some(true) => Ok(format!("Format and check digits pass. {unchecked}")),
        Some(false) => Err(Error::new(
            "vat_check_digits",
            format!("The check digits don't match for {} VAT number.", a(prefix)),
            Some(match source {
                "HMRC" => "Check for a typo, or add --live to ask HMRC.",
                "VIES" => "Check for a typo, or add --live to ask VIES.",
                _ => "Check for a typo, or add --live to ask the UID register.",
            }),
        )),
        None if rest.starts_with(['G', 'H']) => Ok(
            "Format passes. GD and HA numbers have no check digits and no registry to ask.".into(),
        ),
        None => Ok(format!(
            "Format passes; {prefix} numbers have no check digits to test. {unchecked}"
        )),
    };
    Found {
        value: Some(value),
        country: Some(country.into()),
        checks,
        details: json!({"prefix": prefix, "number": number, "branch": branch}),
        outcome,
        warnings: Vec::new(),
    }
}

/// Whether the check digits match, or `None` when the number has none we can test.
fn check_digits(code: &str, r: &str) -> Option<bool> {
    Some(match code {
        "AT" => at(&r[1..]),
        "BE" => 97 - num(&r[..8]) % 97 == num(&r[8..]),
        "BG" => bg(r),
        "CY" => cy(r),
        "DE" => de(r),
        "DK" => weighted(&digits(r), &[2, 7, 6, 5, 4, 3, 2, 1]).is_multiple_of(11),
        "EE" => ee(r),
        "EL" => el(r),
        "ES" => es(r),
        "FI" => fi(r),
        "FR" => fr(r),
        "GB" | "XI" if r.starts_with(['G', 'H']) => return None,
        "GB" | "XI" => gb(r),
        "HR" => hr(r),
        "HU" => hu(r),
        "IE" => ie(r),
        "IT" => it(r),
        "LT" => lt(r),
        "LU" => num(&r[..6]) % 89 == num(&r[6..]),
        "MT" => mt(r),
        "NL" => nl(r),
        "PL" => weighted(&digits(&r[..9]), &[6, 5, 7, 2, 3, 4, 5, 6, 7]) % 11 == last(r),
        "PT" => pt(r),
        "RO" => ro(r),
        "SE" => se(r),
        "SI" => si(r),
        "CH" => uid_check_digit(&digits(&r[1..9])) == digit(r.as_bytes()[9]),
        _ => return None,
    })
}

/// The Swiss UID check digit (eCH-0097) of the first 8 digits; 10 means no UID has them.
pub fn uid_check_digit(first8: &[u32]) -> u32 {
    (11 - weighted(first8, &[5, 4, 3, 2, 7, 6, 5, 4]) % 11) % 11
}

fn digit(b: u8) -> u32 {
    u32::from(b - b'0')
}

fn digits(s: &str) -> Vec<u32> {
    s.bytes().map(digit).collect()
}

fn num(s: &str) -> u32 {
    s.parse().unwrap_or(u32::MAX)
}

fn last(s: &str) -> u32 {
    digit(*s.as_bytes().last().unwrap_or(&b'0'))
}

/// All but the last `n` characters as digits (valvat's `figures`).
fn figures(s: &str, n: usize) -> Vec<u32> {
    digits(&s[..s.len() - n])
}

fn weighted(figs: &[u32], weights: &[u32]) -> u32 {
    figs.iter().zip(weights).map(|(f, w)| f * w).sum()
}

/// Sums `f(index from the right, digit)` (valvat's `sum_figures_by`).
fn sum_by(figs: &[u32], f: impl Fn(u32, u32) -> u32) -> u32 {
    figs.iter().rev().zip(0..).map(|(&d, i)| f(i, d)).sum()
}

/// Luhn-style digit sum, doubling from the right (`reverse`) or from the second from the right.
fn luhn(figs: &[u32], reverse: bool) -> u32 {
    let (even, odd) = if reverse { (2, 1) } else { (1, 2) };
    sum_by(figs, |i, d| {
        let p = d * if i % 2 == 0 { even } else { odd };
        p / 10 + p % 10
    })
}

// at.rb
fn at(r: &str) -> bool {
    (96 - luhn(&figures(r, 1), false)) % 10 == last(r)
}

// bg.rb
fn bg(r: &str) -> bool {
    let f = figures(r, 1);
    if r.len() == 10 {
        let local = weighted(&f, &[2, 4, 8, 5, 10, 9, 7, 3, 6]) % 11;
        let local = if local < 10 { local } else { 0 };
        return local == last(r) || weighted(&f, &[21, 19, 17, 13, 11, 9, 7, 3, 1]) % 10 == last(r);
    }
    let prod = |add: u32| f.iter().zip(add..).map(|(d, w)| d * w).sum::<u32>() % 11;
    let chk = match prod(1) {
        10 => prod(3) % 10,
        c => c,
    };
    chk == last(r)
}

// cy.rb
fn cy(r: &str) -> bool {
    const ODD: [u32; 10] = [1, 0, 5, 7, 9, 13, 15, 17, 19, 21];
    let sum = sum_by(
        &figures(r, 1),
        |i, d| if i % 2 == 1 { ODD[d as usize] } else { d },
    );
    r.as_bytes()[8] == b'A' + (sum % 26) as u8
}

// de.rb
fn de(r: &str) -> bool {
    let prod = figures(r, 1).iter().fold(10, |prod, d| {
        let sum = match (prod + d) % 10 {
            0 => 10,
            s => s,
        };
        2 * sum % 11
    });
    (11 - prod) % 10 == last(r)
}

// ee.rb
fn ee(r: &str) -> bool {
    let sum = sum_by(&figures(r, 1), |i, d| d * [7, 3, 1][i as usize % 3]);
    (10 - sum % 10) % 10 == last(r)
}

// gr.rb
fn el(r: &str) -> bool {
    sum_by(&figures(r, 1), |i, d| d << (i + 1)) % 11 % 10 == last(r)
}

// es.rb
fn es(r: &str) -> bool {
    let b = r.as_bytes();
    let given = b[8];
    let special = b"KLM".contains(&b[0]) && !(1..=56).contains(&num(&r[1..3]));
    if special || ["X0000000T", "00000001R", "00000000T", "99999999R"].contains(&r) {
        return false;
    }
    if b"KLMXYZ".contains(&b[0]) || b[0].is_ascii_digit() {
        let nie = match b[0] {
            b'X' => "0",
            b'Y' => "1",
            b'Z' => "2",
            _ => "",
        };
        let start = if b[0].is_ascii_digit() { 0 } else { 1 };
        let n: u32 = format!("{nie}{}", &r[start..8]).parse().unwrap_or(u32::MAX);
        return given == b"TRWAGMYFPDXBNJZSQVHLCKE"[n as usize % 23];
    }
    let chk = 10 - luhn(&digits(&r[1..8]), true) % 10;
    let letter = !b"HJUV".contains(&b[0]) && given == b'A' + chk as u8 - 1;
    let number = !b"NPQRSW".contains(&b[0]) && given == b'0' + (chk % 10) as u8;
    letter || number
}

// fi.rb
fn fi(r: &str) -> bool {
    (11 - weighted(&figures(r, 1), &[7, 9, 10, 5, 8, 4, 2]) % 11) % 11 == last(r)
}

// fr.rb
fn fr(r: &str) -> bool {
    let siren = u64::from(num(&r[2..]));
    let key = &r.as_bytes()[..2];
    if key.iter().all(u8::is_ascii_digit) {
        return (12 + 3 * (siren % 97)) % 97 == u64::from(num(&r[..2]));
    }
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKLMNPQRSTUVWXYZ";
    let index = |c: u8| ALPHABET.iter().position(|&a| a == c).unwrap_or(0) as u64;
    let check = if key[0].is_ascii_digit() {
        index(key[0]) * 24 + index(key[1]) - 10
    } else {
        index(key[0]) * 34 + index(key[1]) - 100
    };
    (siren + 1 + check / 11) % 11 == check % 11
}

// gb.rb
fn gb(r: &str) -> bool {
    if r.bytes().all(|b| b == b'0') {
        return false;
    }
    let base = num(&r[..7]);
    let sum = weighted(&digits(&r[..7]), &[8, 7, 6, 5, 4, 3, 2]);
    let check = num(&r[7..9]);
    let old = (sum + check).is_multiple_of(97);
    let new = (sum + 55 + check).is_multiple_of(97);
    let old_forbidden = [
        100_000..=999_999,
        9_490_001..=9_700_000,
        9_990_001..=9_999_999,
    ];
    if (old && old_forbidden.iter().any(|r| r.contains(&base)))
        || (new && (1..=1_000_000).contains(&base))
    {
        return false;
    }
    old || new
}

// hr.rb
fn hr(r: &str) -> bool {
    let product = figures(r, 1).iter().fold(10, |product, d| {
        let sum = match (d + product) % 10 {
            0 => 10,
            s => s,
        };
        2 * sum % 11
    });
    (11 - product) % 10 == last(r)
}

// hu.rb
fn hu(r: &str) -> bool {
    let sum = sum_by(&figures(r, 1), |i, d| d * [3, 7, 9, 1][i as usize % 4]);
    (10 - sum % 10) % 10 == last(r)
}

// ie.rb
fn ie(r: &str) -> bool {
    const CHARS: &[u8] = b"WABCDEFGHIJKLMNOPQRSTUV";
    let index = |c: u8| CHARS.iter().position(|&x| x == c);
    // The old format, digit letter 5 digits letter, as the new one.
    let s = match r.as_bytes()[1].is_ascii_uppercase() {
        true => format!("0{}{}{}", &r[2..7], &r[..1], &r[7..]),
        false => r.to_string(),
    };
    let b = s.as_bytes();
    let mut total = sum_by(&digits(&s[..7]), |i, d| d * (i + 2));
    if b.len() == 9 {
        total += index(b[8]).unwrap_or(0) as u32 * 9;
    }
    index(b[7]) == Some(total as usize % 23)
}

// it.rb
fn it(r: &str) -> bool {
    let office = num(&r[7..10]);
    let office_ok = (1..=100).contains(&office) || [120, 121, 999].contains(&office);
    let chk = (10 - luhn(&figures(r, 1), true) % 10) % 10;
    office_ok && &r[..7] != "0000000" && chk == last(r)
}

// lt.rb
fn lt(r: &str) -> bool {
    let f = figures(r, 1);
    let sum_with = |offset: u32| {
        let w = |j: u32| match j + 1 + offset {
            m if m > 9 => m % 9,
            m => m,
        };
        f.iter().zip(0..).map(|(d, j)| d * w(j)).sum::<u32>() % 11
    };
    let chk = [sum_with(0), sum_with(2), 0]
        .into_iter()
        .find(|&c| c != 10)
        .unwrap_or(0);
    chk == last(r)
}

// mt.rb
fn mt(r: &str) -> bool {
    let sum = sum_by(&figures(r, 2), |i, d| d * [9, 8, 7, 6, 4, 3][i as usize]);
    37 - sum % 37 == num(&r[6..])
}

// nl.rb: mod 97 over the whole number (sole traders), else the 11-proof.
fn nl(r: &str) -> bool {
    let mod97 = format!("NL{r}").chars().fold(0, |m, c| {
        let d = c.to_digit(36).unwrap_or(0);
        (m * if d < 10 { 10 } else { 100 } + d) % 97
    });
    mod97 == 1 || sum_by(&digits(&r[..8]), |i, d| d * (i + 2)) % 11 == digit(r.as_bytes()[8])
}

// pt.rb
fn pt(r: &str) -> bool {
    let chk = 11 - sum_by(&figures(r, 1), |i, d| d * (i + 2)) % 11;
    (if chk > 9 { 0 } else { chk }) == last(r)
}

// ro.rb
fn ro(r: &str) -> bool {
    let sum = sum_by(&figures(r, 1), |i, d| {
        d * [2, 3, 5, 7, 1, 2, 3, 5, 7][i as usize]
    });
    sum * 10 % 11 % 10 == last(r)
}

// se.rb
fn se(r: &str) -> bool {
    luhn(&digits(&r[..10]), false).is_multiple_of(10)
}

// si.rb
fn si(r: &str) -> bool {
    let chk = 11 - sum_by(&figures(r, 1), |i, d| d * (i + 2)) % 11;
    !r.starts_with('0') && chk % 10 == last(r) && chk != 11
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::Kind;

    fn vat(raw: &str) -> Found {
        check(&Kind::Vat.normalize(raw))
    }

    /// The same value with its check character changed.
    fn changed(v: &str) -> String {
        let mut b = v.as_bytes().to_vec();
        let i = match &v[..2] {
            "SE" => b.len() - 3,
            "NL" => 10,
            _ => b.len() - 1,
        };
        b[i] = match b[i] {
            b'9' => b'0',
            b'Z' => b'A',
            c => c + 1,
        };
        String::from_utf8(b).unwrap()
    }

    #[test]
    fn real_registered_numbers_pass_and_a_changed_check_digit_fails() {
        // Each was registered in VIES on 2026-10-29 (CZ, LV, SK have no check digits to change).
        let numbers = [
            "ATU66059506",
            "BE0202239951",
            "BG831642181",
            "CY00532445O",
            "CZ00177041",
            "DE811907980",
            "DK61126228",
            "EE100052563",
            "EL094543092",
            "ESA82018474",
            "FI20584306",
            "FR0H384498879",
            "HR06282943396",
            "HU10747759",
            "IE6388047V",
            "IT00197200132",
            "LT213179412",
            "LU17560609",
            "LV40003000642",
            "MT11407334",
            "NL802549391B01",
            "PL5260211587",
            "PT501413197",
            "RO13182060",
            "SE556464687401",
            "SI51049406",
            "SK2020273893",
        ];
        for v in numbers {
            let found = vat(v);
            assert!(found.outcome.is_ok(), "{v}");
            if ["CZ", "LV", "SK"].contains(&&v[..2]) {
                assert_eq!(found.checks[2].pass, None, "{v}");
                continue;
            }
            assert_eq!(found.checks[2].pass, Some(true), "{v}");
            let bad = changed(v);
            assert_eq!(
                vat(&bad).outcome.unwrap_err().code,
                "vat_check_digits",
                "{bad}"
            );
        }
    }

    #[test]
    fn check_digits_agree_with_valvat() {
        // valvat's own spec examples (spec/valvat/checksum/*_spec.rb at commit 5802c029).
        let valid = "ATU13585627 ATU66059506 ATU42403001 BE0123456749 BE0136695962 BE0817331995 \
            BG123456786 BG926067143 BG926067770 BG0101011739 BG0121013021 BG5041019992 BG1521687837 \
            BG1431889037 CY01234567U CY00532445O DE280857971 DE281381706 DE283108332 DE813622378 \
            DE813628528 DE814178359 DE811907980 DK13585628 DK61126228 EE101234568 EE100207415 \
            ESA13585625 ESB83871236 ESE54507058 ES25139013J ESQ1518001A ESQ5018001G ESX4942978W \
            ESX7676464F ESB10317980 ESY3860557K ESY2207765D ES28350472M ES41961720Z ESM1171170X \
            ESK0928769Y ES50095545G ESP6547420G ESC65474207 ESC6547420G FI13669598 FI20584306 \
            FI01080233 FR60528551658 FR43820567501 FR0H384498879 EL123456783 EL094543092 \
            EL998219694 HR06282943396 HR17099025134 HU10672101 HU13460370 HU10747759 IE8473625E \
            IE0123459N IE9B12345N IE1113571MH IE2973912UH IE2974611LH IE2974901UH IE3200115LH \
            IE3206791MH IE3208913KH IE3214048CH IT12345670785 IT01897810162 IT00197200132 \
            IT02762750210 IT00146089990 LT213179412 LT290061371314 LU13669580 LU25361352 \
            LU23124018 LU17560609 MT11407334 MT10126313 MT11539237 NL123456782B12 NL802549391B01 \
            NL808661863B01 NL820893559B01 NL000099998B57 NL123456789B13 PL8567346215 PL5260211587 \
            PL9720575348 PL5272650022 PT136695973 PT501413197 PT503037753 PT500243590 PT500100144 \
            PT502921838 RO123456789 RO99908 RO19 RO124 RO13182060 SE136695975501 SE556464687401 \
            SE502052817901 SE556555952201 SE556084097601 SI59082437 SI51049406 SI86154575 \
            SI47431857 SI22511822 SI26833921 GB727255821 GB434031439 GB434031494";
        for v in valid.split_whitespace() {
            assert_eq!(vat(v).checks[2].pass, Some(true), "{v}");
            let bad = changed(v);
            assert_eq!(vat(&bad).checks.last().unwrap().pass, Some(false), "{bad}");
        }
        let invalid = "ESX65474207 ESP65474207 ESK8201230M ESK0001230B ESX0000000T ES00000001R \
            ES00000000T ES99999999R GB000000000 GB000000000000 GB100000034 GB999999973 IE0000000XX IE0000000AZ \
            IT12345671783 IT00000000133 SE556464687400 SI01111108 SI11111107 PL8566445330 \
            ESP6547420 ESX6547420G";
        for v in invalid.split_whitespace() {
            assert_ne!(vat(v).checks.last().unwrap().pass, Some(true), "{v}");
        }
    }

    #[test]
    fn numbers_without_check_digits() {
        for v in ["CZ12345678", "LV12345678901", "SK1234567890"] {
            let found = vat(v);
            let p = &v[..2];
            let message = format!(
                "Format passes; {p} numbers have no check digits to test. Not checked against VIES; add --live to confirm registration."
            );
            assert_eq!(found.outcome, Ok(message));
            assert_eq!(
                found.checks[2],
                Check {
                    name: "check_digits",
                    pass: None
                }
            );
        }
        for v in ["GBGD100", "GBHA600", "XIGD499"] {
            let found = vat(v);
            assert_eq!(found.checks[2].pass, None, "{v}");
            assert!(
                found
                    .outcome
                    .unwrap()
                    .starts_with("Format passes. GD and HA"),
                "{v}"
            );
        }
    }

    #[test]
    fn details_and_values() {
        let de = vat("de 136 695 976");
        assert_eq!(de.value.as_deref(), Some("DE136695976"));
        assert_eq!(
            de.details,
            json!({"prefix": "DE", "number": "136695976", "branch": null})
        );
        assert_eq!(
            de.outcome,
            Ok("Format and check digits pass. Not checked against VIES; add --live to confirm registration.".into())
        );
        let el = vat("GR094543092");
        assert_eq!(
            (el.value.as_deref(), el.country.as_deref()),
            (Some("EL094543092"), Some("GR"))
        );
        assert_eq!(el.details["prefix"], "EL");
        let gb = vat("GB 434 0314 39 001");
        assert_eq!(
            gb.details,
            json!({"prefix": "GB", "number": "434031439", "branch": "001"})
        );
        assert!(gb.outcome.unwrap().contains("Not checked against HMRC"));
        assert_eq!(vat("XI434031439").country.as_deref(), Some("XI"));
        let ch = vat("CHE-116.281.710 MWST");
        assert_eq!(ch.value.as_deref(), Some("CHE-116.281.710 MWST"));
        assert_eq!(ch.country.as_deref(), Some("CH"));
        assert_eq!(
            ch.details,
            json!({"prefix": "CHE", "number": "116281710", "branch": null})
        );
        assert!(
            ch.outcome
                .unwrap()
                .contains("Not checked against the UID register")
        );
        for v in [
            "CHE-100.000.006 TVA",
            "CHE116281710IVA",
            "che 116 281 710 tpv",
        ] {
            assert!(vat(v).outcome.is_ok(), "{v}");
        }
        let at = vat("ATU66059506");
        assert_eq!(at.details["number"], "U66059506");
    }

    #[test]
    fn every_country_has_its_format() {
        let cases = [
            (
                "ATU1234567",
                "AT VAT numbers are AT followed by U and 8 digits.",
            ),
            (
                "BE2123456789",
                "BE VAT numbers are BE followed by 10 digits starting with 0 or 1.",
            ),
            (
                "BG12345678",
                "BG VAT numbers are BG followed by 9 or 10 digits.",
            ),
            (
                "CY12345678A",
                "CY VAT numbers are CY followed by 8 digits and a letter.",
            ),
            (
                "CY72345678A",
                "CY VAT numbers are CY followed by 8 digits and a letter.",
            ),
            (
                "CZ1234567",
                "CZ VAT numbers are CZ followed by 8, 9 or 10 digits.",
            ),
            ("DE12345678", "DE VAT numbers are DE followed by 9 digits."),
            ("DK1234567", "DK VAT numbers are DK followed by 8 digits."),
            (
                "EE200207415",
                "EE VAT numbers are EE followed by 9 digits starting with 10.",
            ),
            ("EL12345678", "EL VAT numbers are EL followed by 9 digits."),
            (
                "ES123456789",
                "ES VAT numbers are ES followed by 9 characters: a letter or digit, 7 digits, a letter or digit.",
            ),
            ("FI1234567", "FI VAT numbers are FI followed by 8 digits."),
            (
                "FRIO123456789",
                "FR VAT numbers are FR followed by 2 letters or digits and 9 digits.",
            ),
            (
                "HR1234567890",
                "HR VAT numbers are HR followed by 11 digits.",
            ),
            ("HU1234567", "HU VAT numbers are HU followed by 8 digits."),
            (
                "IE1234567",
                "IE VAT numbers are IE followed by 8 or 9 characters: 7 digits and 1 or 2 letters.",
            ),
            (
                "IT1234567890",
                "IT VAT numbers are IT followed by 11 digits.",
            ),
            (
                "LT123456789",
                "LT VAT numbers are LT followed by 9 or 12 digits.",
            ),
            ("LU1234567", "LU VAT numbers are LU followed by 8 digits."),
            (
                "LV1234567890",
                "LV VAT numbers are LV followed by 11 digits.",
            ),
            ("MT1234567", "MT VAT numbers are MT followed by 8 digits."),
            (
                "NL123456789C01",
                "NL VAT numbers are NL followed by 9 digits, B and 2 digits.",
            ),
            (
                "PL123456789",
                "PL VAT numbers are PL followed by 10 digits.",
            ),
            ("PT12345678", "PT VAT numbers are PT followed by 9 digits."),
            (
                "RO0123",
                "RO VAT numbers are RO followed by 2 to 10 digits.",
            ),
            ("RO1", "RO VAT numbers are RO followed by 2 to 10 digits."),
            (
                "SE123456789002",
                "SE VAT numbers are SE followed by 12 digits ending in 01.",
            ),
            ("SI1234567", "SI VAT numbers are SI followed by 8 digits."),
            (
                "SK123456789",
                "SK VAT numbers are SK followed by 10 digits.",
            ),
            (
                "GB1234567890",
                "GB VAT numbers are GB followed by 9 or 12 digits, or GD or HA and 3 digits.",
            ),
            (
                "XIGX123",
                "XI VAT numbers are XI followed by 9 or 12 digits, or GD or HA and 3 digits.",
            ),
            (
                "CHE116281710",
                "CHE VAT numbers are CHE followed by 9 digits and MWST, TVA, IVA or TPV.",
            ),
        ];
        for (v, message) in cases {
            let found = vat(v);
            let e = found.outcome.expect_err(v);
            assert_eq!((e.code, e.message.as_str()), ("vat_format", message), "{v}");
            assert_eq!(found.checks.len(), 2);
        }
    }

    #[test]
    fn prefixes_and_check_digit_errors() {
        let e = vat("136695976").outcome.unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            ("vat_prefix", "A VAT number needs its country prefix.")
        );
        let e = vat("US136695976").outcome.unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            ("vat_country", "No VAT rules for US.")
        );
        let e = vat("DE136695977").outcome.unwrap_err();
        assert_eq!(
            e.message,
            "The check digits don't match for a DE VAT number."
        );
        assert_eq!(e.hint, Some("Check for a typo, or add --live to ask VIES."));
        let e = vat("EL094543093").outcome.unwrap_err();
        assert_eq!(
            e.message,
            "The check digits don't match for an EL VAT number."
        );
        assert_eq!(
            vat("GB434031438").outcome.unwrap_err().hint,
            Some("Check for a typo, or add --live to ask HMRC.")
        );
        let e = vat("CHE-116.281.711 MWST").outcome.unwrap_err();
        assert_eq!(
            e.message,
            "The check digits don't match for a CHE VAT number."
        );
    }

    #[test]
    fn uid_check_digits() {
        assert_eq!(uid_check_digit(&digits("11628171")), 0);
        assert_eq!(uid_check_digit(&digits("10000000")), 6);
    }
}
