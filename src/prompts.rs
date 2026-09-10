use std::fmt::Display;
use std::io::{self, IsTerminal, Write};

use inquire::{InquireError, Select};

/// Remove a prompt whose answer formatter is empty. Inquire leaves the cursor
/// after its final line, even if the menu scrolled the terminal. A saved screen
/// position is only a fallback for errors that did not render a final line.
pub fn run<T>(
    message: &str,
    prompt: impl FnOnce() -> Result<T, InquireError>,
) -> Result<T, InquireError> {
    let terminal = io::stdin().is_terminal() && io::stderr().is_terminal();
    if terminal {
        io::stdout().flush()?;
        eprint!("\x1b7");
        io::stderr().flush()?;
    }
    let result = prompt();
    if terminal {
        if result.is_ok() || matches!(result, Err(InquireError::OperationCanceled)) {
            let suffix = if result.is_ok() { "" } else { "<canceled>" };
            let width = usize::from(crossterm::terminal::size()?.0.max(1));
            let mut rows = 1;
            let mut column = 0;
            for character in format!("? {message} {suffix}").chars() {
                let cells = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
                if character == '\n' || column + cells > width {
                    rows += 1;
                    column = 0;
                }
                if character != '\n' {
                    column += cells;
                }
            }
            eprint!("\x1b[{rows}A\r\x1b[J");
        } else {
            eprint!("\x1b8\r\x1b[J");
        }
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
    let result = run(message, || {
        prompt.with_formatter(&|_| String::new()).raw_prompt()
    });
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

/// Reads cancellation keys while an operation owns the terminal.
pub struct CancellationInput {
    terminal: bool,
}

impl CancellationInput {
    pub fn new() -> anyhow::Result<Self> {
        let input = Self {
            terminal: io::stdin().is_terminal() && io::stdout().is_terminal(),
        };
        input.resume()?;
        Ok(input)
    }

    pub fn pause(&self) -> anyhow::Result<()> {
        if self.terminal {
            crossterm::terminal::disable_raw_mode()?;
        }
        Ok(())
    }

    pub fn resume(&self) -> anyhow::Result<()> {
        if !self.terminal {
            return Ok(());
        }
        // Keep normal newline output while reading individual keys without echo.
        #[cfg(unix)]
        let output_flags = {
            let mut mode = std::mem::MaybeUninit::<libc::termios>::uninit();
            if unsafe { libc::tcgetattr(libc::STDIN_FILENO, mode.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error().into());
            }
            unsafe { mode.assume_init() }.c_oflag
        };
        crossterm::terminal::enable_raw_mode()?;
        #[cfg(unix)]
        {
            let mut mode = std::mem::MaybeUninit::<libc::termios>::uninit();
            if unsafe { libc::tcgetattr(libc::STDIN_FILENO, mode.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error().into());
            }
            let mut mode = unsafe { mode.assume_init() };
            mode.c_oflag = output_flags;
            if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &mode) } != 0 {
                return Err(io::Error::last_os_error().into());
            }
        }
        Ok(())
    }

    pub async fn cancelled(&self, ctrl_c: bool) -> anyhow::Result<()> {
        if !self.terminal {
            tokio::signal::ctrl_c().await?;
            return Ok(());
        }
        loop {
            while crossterm::event::poll(std::time::Duration::ZERO)? {
                if matches!(crossterm::event::read()?, crossterm::event::Event::Key(key)
                    if key.kind != crossterm::event::KeyEventKind::Release
                        && (key.code == crossterm::event::KeyCode::Esc
                            || (ctrl_c
                                && key.code == crossterm::event::KeyCode::Char('c')
                                && key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL))))
                {
                    return Ok(());
                }
            }
            tokio::select! {
                result = tokio::signal::ctrl_c(), if ctrl_c => {
                    result?;
                    return Ok(());
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {}
            }
        }
    }
}

impl Drop for CancellationInput {
    fn drop(&mut self) {
        // Keys typed during this operation must not answer the next prompt.
        if self.terminal {
            while let Ok(true) = crossterm::event::poll(std::time::Duration::ZERO) {
                if crossterm::event::read().is_err() {
                    break;
                }
            }
        }
        let _ = self.pause();
    }
}

#[derive(Debug)]
pub struct Cancelled;

impl Display for Cancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("operation cancelled")
    }
}

impl std::error::Error for Cancelled {}
