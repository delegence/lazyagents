use std::io::{self, IsTerminal};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

/// Shows temporary progress in a terminal, or a plain status line in a pipe.
pub fn start(message: String) -> ProgressBar {
    if !io::stderr().is_terminal() {
        eprintln!("{message}");
        return ProgressBar::hidden();
    }
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::with_template("{spinner} {msg}")
            .expect("the static spinner template must be valid"),
    );
    spinner.set_message(message);
    spinner.enable_steady_tick(Duration::from_millis(100));
    spinner
}
