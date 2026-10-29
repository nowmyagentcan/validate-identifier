const PAGE: &str = include_str!("../src/ui.html");

#[test]
fn page_reaches_no_other_host() {
    let allowed = [
        "https://nowmyagentcan.com",
        "https://github.com/nowmyagentcan/validate-identifier",
        "https://x.com/chayp05",
        "http://www.w3.org/2000/svg",
        "https://smp.example.net/",
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
