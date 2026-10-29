fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("--version" | "-V") => println!("{}", validate_identifier::version()),
        _ => {
            eprintln!("Usage: validate-identifier --version");
            std::process::exit(2);
        }
    }
}
