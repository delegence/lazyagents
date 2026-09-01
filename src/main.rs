mod app;
mod cli;
mod file_system;
mod harness;
mod integrations;
mod profile;
mod yaml;

fn main() {
    if let Err(error) = cli::run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
