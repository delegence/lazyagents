use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event};
use tokio::sync::oneshot;

use super::editor::{Action, Editor};
use super::layout::Display;

pub const DARK_GRAY: &str = "38;5;242";

pub struct ChatHeader {
    name: String,
    harness: String,
    model: String,
    thinking: String,
    permissions: crate::permissions::Mode,
    name_color: crate::config::AgentColor,
}

impl ChatHeader {
    pub fn new(
        name: &str,
        name_color: crate::config::AgentColor,
        harness: &str,
        model: &str,
        thinking: &str,
        permissions: crate::permissions::Mode,
    ) -> Self {
        Self {
            name: crate::markdown::sanitize(name),
            harness: crate::markdown::sanitize(harness),
            model: crate::markdown::sanitize(&model.to_lowercase()),
            thinking: crate::markdown::sanitize(thinking),
            permissions,
            name_color,
        }
    }

    pub fn render(&self, colored: bool) -> String {
        let paint = |color: &str, text: &str| {
            if colored {
                format!("\x1b[{color}m{text}\x1b[0m")
            } else {
                text.to_owned()
            }
        };
        let rule = paint(DARK_GRAY, "───");
        let mut text = format!(
            "{rule}\n{}\n{rule}\n",
            paint(self.name_color.ansi(), &self.name)
        );
        text.push_str(&paint("38;5;250", "harness: "));
        text.push_str(&paint("93", &self.harness));
        text.push_str(&paint("35", &format!(" ({})", self.permissions)));
        text.push('\n');
        text.push_str(&paint("38;5;250", "model: "));
        text.push_str(&paint("93", &self.model));
        text.push_str(&paint("35", &format!(" ({})", self.thinking)));
        text.push_str(&format!("\n{rule}\n\n"));
        text
    }
}

#[derive(Clone)]
struct DraftWriter(mpsc::Sender<DraftUpdate>);

enum DraftUpdate {
    Changed(String),
    Flush(String, mpsc::Sender<Result<()>>),
}

impl DraftWriter {
    fn new(root: PathBuf) -> Self {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut pending = None;
            let mut saved = crate::config::AgentConfig::load(&root)
                .map(|config| config.draft)
                .unwrap_or_default();
            loop {
                match receiver.recv_timeout(Duration::from_millis(400)) {
                    Ok(DraftUpdate::Changed(text)) => {
                        if text.is_empty() && saved != text {
                            if crate::config::save_draft(&root, &text).is_ok() {
                                saved = text;
                                pending = None;
                            } else {
                                pending = Some(text);
                            }
                        } else {
                            pending = Some(text);
                        }
                    }
                    Ok(DraftUpdate::Flush(text, reply)) => {
                        pending = None;
                        let result = crate::config::save_draft(&root, &text);
                        if result.is_ok() {
                            saved = text;
                        }
                        let _ = reply.send(result);
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if let Some(text) = pending.take().filter(|text| text != &saved) {
                            match crate::config::save_draft(&root, &text) {
                                Ok(()) => saved = text,
                                Err(_) => pending = Some(text),
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        if let Some(text) = pending {
                            let _ = crate::config::save_draft(&root, &text);
                        }
                        break;
                    }
                }
            }
        });
        Self(sender)
    }

    fn changed(&self, text: &str) {
        let _ = self.0.send(DraftUpdate::Changed(text.to_owned()));
    }

    fn flush(&self, text: &str) -> Result<()> {
        let (sender, receiver) = mpsc::channel();
        self.0
            .send(DraftUpdate::Flush(text.to_owned(), sender))
            .context("draft writer stopped")?;
        receiver.recv().context("draft writer stopped")?
    }
}

pub enum Input {
    Line(String),
    Interrupted,
    Eof,
}

/// Reads only on request. Permission prompts retain exclusive terminal access
/// between turns, and a blocked pipe read does not hold up Tokio shutdown.
pub struct ChatInput {
    requests: mpsc::Sender<oneshot::Sender<Result<Input>>>,
    draft: Option<DraftWriter>,
}

impl ChatInput {
    pub fn new(history: Vec<String>, root: PathBuf, header: Option<ChatHeader>) -> Self {
        let (requests, receiver) = mpsc::channel::<oneshot::Sender<Result<Input>>>();
        let draft = (io::stdin().is_terminal() && io::stdout().is_terminal())
            .then(|| DraftWriter::new(root.clone()));
        let writer = draft.clone();
        std::thread::spawn(move || {
            let mut editor = None;
            let mut header = header;
            while let Ok(reply) = receiver.recv() {
                let result = (|| -> Result<Input> {
                    if io::stdin().is_terminal() && io::stdout().is_terminal() {
                        if editor.is_none() {
                            editor = Some((
                                Editor::new(
                                    history.clone(),
                                    crate::config::AgentConfig::load(&root)?.draft,
                                ),
                                writer.as_ref().expect("interactive draft writer").clone(),
                            ));
                        }
                        let (editor, draft) = editor.as_mut().expect("editor initialized above");
                        read_terminal(editor, draft, header.take())
                    } else {
                        eprint!("> ");
                        io::stderr().flush()?;
                        let mut line = String::new();
                        let count = io::stdin().read_line(&mut line)?;
                        Ok(if count == 0 {
                            Input::Eof
                        } else {
                            Input::Line(line)
                        })
                    }
                })();
                if reply.send(result).is_err() {
                    break;
                }
            }
        });
        Self { requests, draft }
    }

    /// Clear only after the caller has saved the prompt or accepted a command.
    pub fn accepted(&self) -> Result<()> {
        if let Some(draft) = &self.draft {
            draft.flush("")?;
        }
        Ok(())
    }

    pub async fn read_line(&self) -> Result<Input> {
        let (reply, receiver) = oneshot::channel();
        self.requests.send(reply).context("input reader stopped")?;
        receiver.await.context("input reader stopped")?
    }
}

// Each read owns raw mode and paste mode. Drop also restores them on an I/O error.
struct TerminalMode;

impl TerminalMode {
    fn enter() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        let mode = Self;
        crossterm::execute!(io::stderr(), event::EnableBracketedPaste)?;
        Ok(mode)
    }
}

impl Drop for TerminalMode {
    fn drop(&mut self) {
        let _ = io::stderr().write_all(b"\x1b]133;C\x07");
        let _ = crossterm::execute!(
            io::stderr(),
            event::DisableBracketedPaste,
            crossterm::terminal::EnableLineWrap,
            crossterm::style::SetAttribute(crossterm::style::Attribute::Reset),
            crossterm::cursor::Show,
            crossterm::terminal::EndSynchronizedUpdate
        );
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

fn read_terminal(
    editor: &mut Editor,
    draft: &DraftWriter,
    header: Option<ChatHeader>,
) -> Result<Input> {
    io::stdout().flush()?;
    let mut size = crossterm::terminal::size()?;
    // The header is normal output, not part of the editable region. Print it
    // once so a resize can never leave multiple copies in terminal scrollback.
    if let Some(header) = header {
        eprint!("{}", header.render(true));
        io::stderr().flush()?;
    }
    let _mode = TerminalMode::enter()?;
    let mut display = Display::new();
    let result = (|| {
        display.draw(&mut io::stderr(), editor, size)?;
        loop {
            let event = match event::read() {
                Ok(event) => event,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    draft.flush(&editor.text)?;
                    display.finish(&mut io::stderr(), "", size)?;
                    return Ok(Input::Interrupted);
                }
                Err(error) => return Err(error.into()),
            };
            let revision = editor.revision();
            let resized = matches!(event, Event::Resize(_, _));
            let mut action = match event {
                Event::Key(key) => editor.key(key),
                Event::Paste(text) => {
                    editor.insert(&text);
                    Action::Redraw
                }
                Event::Resize(_, _) => Action::Redraw,
                _ => Action::None,
            };
            // A key can arrive before its resize event. Also redraw when sizes
            // match: a shrink and grow can be coalesced into one final signal.
            let current = crossterm::terminal::size()?;
            if resized || current != size {
                display.resized(&mut io::stderr())?;
                size = current;
                if matches!(action, Action::None) {
                    action = Action::Redraw;
                }
            }
            match action {
                Action::None => continue,
                Action::Redraw => {
                    if revision != editor.revision() {
                        draft.changed(&editor.text);
                    }
                    display.draw(&mut io::stderr(), editor, size)?;
                }
                Action::Submit => {
                    let line = editor.submit();
                    // Keep a recoverable copy until the caller saves the session entry.
                    draft.flush(&line)?;
                    display.finish(&mut io::stderr(), &line, size)?;
                    return Ok(Input::Line(line));
                }
                Action::Eof => {
                    draft.flush("")?;
                    display.finish(&mut io::stderr(), "", size)?;
                    return Ok(Input::Eof);
                }
            }
        }
    })();
    if result.is_err() {
        let _ = display.finish(&mut io::stderr(), "", size);
    }
    result
}
