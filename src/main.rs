fn main() {
    if let Err(err) = mews::cli::run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}
