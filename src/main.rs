fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("--version" | "-V") => println!("validate-identifier {}", env!("CARGO_PKG_VERSION")),
        _ => {
            eprintln!("Usage: validate-identifier --version");
            std::process::exit(2);
        }
    }
}
