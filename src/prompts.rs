use std::fmt::Display;
use std::io::{self, IsTerminal, Write};

use inquire::{InquireError, Select};

/// Each prompt owns the terminal until it returns. Discard its temporary rows
/// from the saved start, including when Inquire's resize offsets are stale.
pub fn run<T>(prompt: impl FnOnce() -> Result<T, InquireError>) -> Result<T, InquireError> {
    let terminal = io::stdin().is_terminal() && io::stderr().is_terminal();
    if terminal {
        io::stdout().flush()?;
        eprint!("\x1b7");
        io::stderr().flush()?;
    }
    let result = prompt();
    if terminal {
        eprint!("\x1b8\r\x1b[J");
        io::stderr().flush()?;
    }
    result
}

/// Print the answer as normal output after removing the interactive menu.
pub fn select<T: Display>(prompt: Select<'_, T>) -> Result<T, InquireError> {
    let message = prompt.message;
    let formatter = prompt.formatter;
    let prefix = prompt.render_config.answered_prompt_prefix;
    let colored = !prompt.render_config.answer.is_empty();
    let result = run(|| prompt.raw_prompt());
    if let Ok(answer) = &result {
        let answer = crate::markdown::sanitize(&formatter(answer.as_ref()));
        let message = crate::markdown::sanitize(message);
        if colored {
            let prefix_color = if prefix.style.fg.is_some() { "92" } else { "0" };
            eprintln!(
                "\x1b[{prefix_color}m{}\x1b[0m {message} \x1b[38;5;250m{answer}\x1b[0m",
                prefix.content
            );
        } else {
            eprintln!("{} {message} {answer}", prefix.content);
        }
    }
    result.map(|answer| answer.value)
}

pub fn theme() -> inquire::ui::RenderConfig<'static> {
    use inquire::ui::{Color, RenderConfig};

    let mut theme = RenderConfig::default();
    for style in [
        &mut theme.help_message,
        &mut theme.answer,
        &mut theme.highlighted_option_prefix.style,
        &mut theme.unhighlighted_option_prefix.style,
    ] {
        if matches!(style.fg, Some(Color::LightCyan | Color::DarkCyan)) {
            style.fg = Some(Color::AnsiValue(250));
        }
    }
    if let Some(selected) = &mut theme.selected_option {
        selected.fg = Some(Color::AnsiValue(15));
        theme.option.fg = Some(Color::AnsiValue(250));
        for style in [
            &mut theme.highlighted_option_prefix.style,
            &mut theme.scroll_up_prefix.style,
            &mut theme.scroll_down_prefix.style,
        ] {
            style.fg = Some(Color::AnsiValue(15));
        }
    }
    theme
}
