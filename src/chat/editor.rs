use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::commands::{Command, CommandSpec, COMMANDS};

pub(super) enum Action {
    None,
    Redraw,
    Submit,
    Eof,
}

pub(super) struct Editor {
    pub text: String,
    pub cursor: usize,
    pub selected: usize,
    dismissed: bool,
    revision: u64,
    history: Vec<String>,
    history_index: usize,
    saved_draft: String,
    kill: String,
    undo: Vec<(String, usize)>,
    search: Option<Search>,
}

struct Search {
    query: String,
    original: (String, usize, usize),
    index: usize,
    reverse: bool,
    failed: bool,
}

impl Editor {
    pub fn new(mut history: Vec<String>, text: String) -> Self {
        for line in &mut history {
            *line = clean(line);
        }
        history.retain(|line| !line.trim().is_empty() && Command::parse(line.trim()).is_none());
        history.dedup();
        history.drain(..history.len().saturating_sub(1000));
        let text = clean(&text);
        Self {
            cursor: text.len(),
            text,
            selected: 0,
            dismissed: false,
            revision: 0,
            history_index: history.len(),
            history,
            saved_draft: String::new(),
            kill: String::new(),
            undo: Vec::new(),
            search: None,
        }
    }

    pub fn choices(&self) -> Vec<&'static CommandSpec> {
        if self.search.is_some()
            || self.dismissed
            || !self.text.starts_with('/')
            || self.text.chars().any(char::is_whitespace)
        {
            return Vec::new();
        }
        COMMANDS
            .iter()
            .filter(|command| command.name.starts_with(&self.text))
            .collect()
    }

    fn checkpoint(&mut self) {
        if self.undo.len() == 100 {
            self.undo.remove(0);
        }
        self.undo.push((self.text.clone(), self.cursor));
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.selected = 0;
        self.dismissed = false;
        // Insertion can join adjacent graphemes, including a combining mark or ZWJ.
        self.cursor = self
            .text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain(std::iter::once(self.text.len()))
            .find(|i| *i >= self.cursor)
            .unwrap();
    }

    pub fn insert(&mut self, text: &str) {
        let text = clean(text);
        if text.is_empty() {
            return;
        }
        if let Some(search) = &mut self.search {
            search.query.push_str(&text);
            self.find_history(false);
            return;
        }
        self.checkpoint();
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
        self.changed();
    }

    fn previous(&self) -> usize {
        self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next(&self) -> usize {
        self.cursor
            + self.text[self.cursor..]
                .graphemes(true)
                .next()
                .map_or(0, str::len)
    }

    fn word_left(&self) -> usize {
        let mut i = self.cursor;
        let mut word = false;
        for (start, g) in self.text[..self.cursor].grapheme_indices(true).rev() {
            if g.chars().all(char::is_whitespace) {
                if word {
                    break;
                }
            } else {
                word = true;
            }
            i = start;
        }
        i
    }

    fn word_right(&self) -> usize {
        let mut i = self.cursor;
        let mut word = false;
        for (start, g) in self.text[self.cursor..].grapheme_indices(true) {
            if g.chars().all(char::is_whitespace) {
                if word {
                    break;
                }
            } else {
                word = true;
            }
            i = self.cursor + start + g.len();
        }
        i
    }

    fn remove(&mut self, start: usize, end: usize, kill: bool) {
        if start == end {
            return;
        }
        self.checkpoint();
        if kill {
            self.kill = self.text[start..end].to_owned();
        }
        self.text.replace_range(start..end, "");
        self.cursor = start;
        self.changed();
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i)
    }

    fn history(&mut self, up: bool) {
        let index = if up {
            self.history_index.saturating_sub(1)
        } else {
            (self.history_index + 1).min(self.history.len())
        };
        if index == self.history_index {
            return;
        }
        if self.history_index == self.history.len() {
            self.saved_draft = self.text.clone();
        }
        self.checkpoint();
        self.history_index = index;
        self.text = self.history.get(index).unwrap_or(&self.saved_draft).clone();
        self.cursor = self.text.len();
        self.changed();
    }

    fn vertical(&mut self, up: bool) {
        let start = self.line_start();
        let end = self.line_end();
        let target = if up && start > 0 {
            let end = start - 1;
            Some((self.text[..end].rfind('\n').map_or(0, |i| i + 1), end))
        } else if !up && end < self.text.len() {
            let start = end + 1;
            Some((
                start,
                self.text[start..]
                    .find('\n')
                    .map_or(self.text.len(), |i| start + i),
            ))
        } else {
            None
        };
        if let Some((a, b)) = target {
            let column = display_column(
                &self.text[start..self.cursor],
                if start == 0 { 2 } else { 0 },
            );
            let mut width = if a == 0 { 2 } else { 0 };
            self.cursor = a;
            for g in self.text[a..b].graphemes(true) {
                width = display_column(g, width);
                if width > column {
                    break;
                }
                self.cursor += g.len();
            }
        } else {
            self.history(up);
        }
    }

    pub fn prompt(&self) -> String {
        self.search.as_ref().map_or_else(
            || "> ".into(),
            |search| {
                let failed = if search.failed { "failed " } else { "" };
                let direction = if search.reverse { "reverse" } else { "forward" };
                format!("({failed}{direction}-i-search)`{}': ", search.query)
            },
        )
    }

    fn find_history(&mut self, advance: bool) {
        let search = self.search.as_mut().unwrap();
        let start = if advance {
            if search.reverse {
                search.index.checked_sub(1)
            } else {
                search.index.checked_add(1)
            }
        } else {
            Some(search.index)
        };
        let found = start.and_then(|start| {
            let mut indices = (0..self.history.len()).filter(|i| {
                if search.reverse {
                    *i <= start
                } else {
                    *i >= start
                }
            });
            if search.reverse {
                indices.rfind(|i| self.history[*i].contains(&search.query))
            } else {
                indices.find(|i| self.history[*i].contains(&search.query))
            }
        });
        search.failed = found.is_none();
        if let Some(index) = found {
            search.index = index;
            self.history_index = index;
            self.text = self.history[index].clone();
            self.revision = self.revision.wrapping_add(1);
            let offset = self.text.find(&search.query).unwrap();
            // A substring can start inside a grapheme. Keep the edit cursor safe.
            self.cursor = self
                .text
                .grapheme_indices(true)
                .map(|(i, _)| i)
                .take_while(|i| *i <= offset)
                .last()
                .unwrap_or(0);
        }
    }

    fn search_key(&mut self, key: KeyEvent) -> Option<Action> {
        use KeyCode::*;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if ctrl && matches!(key.code, Char('r' | 's')) {
            if self.history.is_empty() {
                return Some(Action::None);
            }
            let advance = self.search.is_some();
            if !advance {
                self.checkpoint();
                if self.history_index == self.history.len() {
                    self.saved_draft = self.text.clone();
                }
                self.search = Some(Search {
                    query: String::new(),
                    original: (self.text.clone(), self.cursor, self.history_index),
                    index: if key.code == Char('r') {
                        self.history.len() - 1
                    } else {
                        0
                    },
                    reverse: true,
                    failed: false,
                });
            }
            self.search.as_mut().unwrap().reverse = key.code == Char('r');
            self.find_history(advance);
            return Some(Action::Redraw);
        }
        self.search.as_ref()?;
        match key.code {
            Char('g' | 'c') if ctrl => {
                let search = self.search.take().unwrap();
                (self.text, self.cursor, self.history_index) = search.original;
                self.revision = self.revision.wrapping_add(1);
                Some(Action::Redraw)
            }
            Backspace | Char('h') if key.code == Backspace || ctrl => {
                let query = &mut self.search.as_mut().unwrap().query;
                let end = query
                    .grapheme_indices(true)
                    .next_back()
                    .map_or(0, |(i, _)| i);
                query.truncate(end);
                self.find_history(false);
                Some(Action::Redraw)
            }
            Char(c) if !ctrl && !alt => {
                self.insert(&c.to_string());
                Some(Action::Redraw)
            }
            Esc => {
                self.search = None;
                Some(Action::Redraw)
            }
            _ => {
                self.search = None;
                None
            }
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> Action {
        use KeyCode::*;
        if key.kind == KeyEventKind::Release {
            return Action::None;
        }
        if let Some(action) = self.search_key(key) {
            return action;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if (key.code == Enter && alt) || (key.code == Char('j') && ctrl) {
            self.insert("\n");
            return Action::Redraw;
        }
        let choices = self.choices();
        if !ctrl && !alt && !choices.is_empty() {
            match key.code {
                Up => self.selected = (self.selected + choices.len() - 1) % choices.len(),
                Down => self.selected = (self.selected + 1) % choices.len(),
                Esc => self.dismissed = true,
                Tab => {
                    let name = choices[self.selected].name;
                    if self.text != name {
                        self.checkpoint();
                        self.text = name.to_owned();
                        self.changed();
                    }
                    self.cursor = self.text.len();
                }
                Enter => return Action::Submit,
                _ => return self.edit_key(key),
            }
            return Action::Redraw;
        }
        self.edit_key(key)
    }

    fn edit_key(&mut self, key: KeyEvent) -> Action {
        use KeyCode::*;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            Enter => return Action::Submit,
            Esc | Char('c') if key.code == Esc || ctrl => self.remove(0, self.text.len(), true),
            Char('d') if ctrl && self.text.is_empty() => return Action::Eof,
            Char('d') if ctrl => self.remove(self.cursor, self.next(), false),
            Backspace if alt || ctrl => self.remove(self.word_left(), self.cursor, true),
            Backspace | Char('h') if key.code == Backspace || ctrl => {
                self.remove(self.previous(), self.cursor, false)
            }
            Delete => self.remove(
                self.cursor,
                if ctrl { self.word_right() } else { self.next() },
                false,
            ),
            Left if ctrl || alt => self.cursor = self.word_left(),
            Right if ctrl || alt => self.cursor = self.word_right(),
            Left | Char('b') if key.code == Left || ctrl => self.cursor = self.previous(),
            Right | Char('f') if key.code == Right || ctrl => self.cursor = self.next(),
            Char('b') if alt => self.cursor = self.word_left(),
            Char('f') if alt => self.cursor = self.word_right(),
            Char('d') if alt => self.remove(self.cursor, self.word_right(), true),
            Home | Char('a') if key.code == Home || ctrl => {
                self.cursor = if ctrl && key.code == Home {
                    0
                } else {
                    self.line_start()
                }
            }
            End | Char('e') if key.code == End || ctrl => {
                self.cursor = if ctrl && key.code == End {
                    self.text.len()
                } else {
                    self.line_end()
                }
            }
            Up => self.vertical(true),
            Down => self.vertical(false),
            Char('p') if ctrl => self.history(true),
            Char('n') if ctrl => self.history(false),
            Char('<') if alt => self.cursor = 0,
            Char('>') if alt => self.cursor = self.text.len(),
            Char('u') if ctrl => self.remove(self.line_start(), self.cursor, true),
            Char('k') if ctrl => self.remove(
                self.cursor,
                if self.cursor == self.line_end() {
                    self.next()
                } else {
                    self.line_end()
                },
                true,
            ),
            Char('w') if ctrl => self.remove(self.word_left(), self.cursor, true),
            Char('y') if ctrl => self.insert(&self.kill.clone()),
            Char('_') if ctrl => {
                if let Some((text, cursor)) = self.undo.pop() {
                    self.text = text;
                    self.cursor = cursor;
                    self.changed();
                }
            }
            Char('t') if ctrl => {
                if self.cursor > 0 {
                    let end = if self.cursor == self.text.len() {
                        self.cursor
                    } else {
                        self.next()
                    };
                    let right = self.text[..end]
                        .grapheme_indices(true)
                        .next_back()
                        .unwrap()
                        .0;
                    if let Some((left, _)) = self.text[..right].grapheme_indices(true).next_back() {
                        self.checkpoint();
                        let swapped =
                            format!("{}{}", &self.text[right..end], &self.text[left..right]);
                        self.text.replace_range(left..end, &swapped);
                        self.cursor = end;
                        self.changed();
                    }
                }
            }
            Char('l') if ctrl => {}
            Char(c) if !ctrl && !alt => self.insert(&c.to_string()),
            Tab => self.insert("\t"),
            _ => return Action::None,
        }
        Action::Redraw
    }

    pub fn submit(&mut self) -> String {
        let selected = self
            .choices()
            .get(self.selected)
            .map(|choice| choice.command);
        let line = selected.map_or_else(|| self.text.clone(), |command| command.name().to_owned());
        if !line.trim().is_empty()
            && Command::parse(line.trim()).is_none()
            && self.history.last() != Some(&line)
        {
            self.history.push(line.clone());
            if self.history.len() > 1000 {
                self.history.remove(0);
            }
        }
        self.text.clear();
        self.cursor = 0;
        self.history_index = self.history.len();
        self.saved_draft.clear();
        self.undo.clear();
        self.changed();
        line
    }
}

fn display_column(text: &str, initial: usize) -> usize {
    text.graphemes(true).fold(initial, |column, g| {
        column
            + if g == "\t" {
                8 - column % 8
            } else {
                UnicodeWidthStr::width(g).max(1)
            }
    })
}

fn clean(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(editor: &mut Editor, code: KeyCode, modifiers: KeyModifiers) -> Action {
        let action = editor.key(KeyEvent::new(code, modifiers));
        assert!(editor
            .text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain(std::iter::once(editor.text.len()))
            .any(|i| i == editor.cursor));
        action
    }

    #[test]
    fn history_search_cycles_cancels_and_keeps_grapheme_boundaries() {
        use KeyCode::*;
        let mut editor = Editor::new(
            vec![
                "first café".into(),
                "other".into(),
                "last cafe\u{301}".into(),
            ],
            "draft".into(),
        );
        key(&mut editor, Char('r'), KeyModifiers::CONTROL);
        editor.insert("caf");
        assert_eq!(editor.text, "last cafe\u{301}");
        key(&mut editor, Char('r'), KeyModifiers::CONTROL);
        assert_eq!(editor.text, "first café");
        key(&mut editor, Char('s'), KeyModifiers::CONTROL);
        assert_eq!(editor.text, "last cafe\u{301}");
        editor.insert("missing");
        assert!(editor.prompt().contains("failed"));
        key(&mut editor, Char('g'), KeyModifiers::CONTROL);
        assert_eq!(editor.text, "draft");
        assert_eq!(editor.cursor, 5);
        key(&mut editor, Char('r'), KeyModifiers::CONTROL);
        editor.insert("\u{301}");
        assert_eq!(editor.cursor, "last caf".len());
        key(&mut editor, Backspace, KeyModifiers::NONE);
        assert!(!editor.prompt().contains("failed"));
        key(&mut editor, Esc, KeyModifiers::NONE);
        assert_eq!(editor.prompt(), "> ");
        key(&mut editor, Char('n'), KeyModifiers::CONTROL);
        assert_eq!(editor.text, "draft");
        key(&mut editor, Char('r'), KeyModifiers::CONTROL);
        editor.insert("other");
        assert!(matches!(
            key(&mut editor, Enter, KeyModifiers::NONE),
            Action::Submit
        ));
        assert_eq!(editor.submit(), "other");
    }

    #[test]
    fn paste_multiline_and_grapheme_edits() {
        use KeyCode::*;
        let mut editor = Editor::new(vec![], String::new());
        editor.insert("e\u{301}👩🏽‍💻🇺🇿\r\n界\rline\x1b\x07");
        assert_eq!(editor.text, "e\u{301}👩🏽‍💻🇺🇿\n界\nline");
        key(&mut editor, Home, KeyModifiers::CONTROL);
        key(&mut editor, Right, KeyModifiers::NONE);
        assert_eq!(editor.cursor, "e\u{301}".len());
        key(&mut editor, Delete, KeyModifiers::NONE);
        assert!(editor.text.starts_with("e\u{301}🇺🇿"));
        key(&mut editor, Backspace, KeyModifiers::NONE);
        assert!(editor.text.starts_with("🇺🇿"));
        key(&mut editor, Enter, KeyModifiers::ALT);
        key(&mut editor, Char('j'), KeyModifiers::CONTROL);
        assert!(editor.text.starts_with("\n\n🇺🇿"));
        editor.text = "👩💻".into();
        editor.cursor = "👩".len();
        editor.insert("\u{200d}");
        assert_eq!(editor.cursor, editor.text.len());
        key(&mut editor, Backspace, KeyModifiers::NONE);
        assert_eq!(editor.text, "");
    }

    #[test]
    fn history_restores_the_draft_and_excludes_commands() {
        use KeyCode::*;
        let mut editor = Editor::new(
            vec!["first\nsecond".into(), "/status".into(), "last".into()],
            "draft".into(),
        );
        key(&mut editor, Up, KeyModifiers::NONE);
        assert_eq!(editor.text, "last");
        key(&mut editor, Up, KeyModifiers::NONE);
        assert_eq!(editor.text, "first\nsecond");
        key(&mut editor, Char('n'), KeyModifiers::CONTROL);
        key(&mut editor, Down, KeyModifiers::NONE);
        assert_eq!(editor.text, "draft");
        editor.submit();
        key(&mut editor, Up, KeyModifiers::NONE);
        assert_eq!(editor.text, "draft");
    }

    #[test]
    fn command_selection_dismissal_and_submission() {
        use KeyCode::*;
        let mut editor = Editor::new(vec![], "/".into());
        assert_eq!(editor.choices().len(), COMMANDS.len());
        key(&mut editor, Up, KeyModifiers::NONE);
        assert_eq!(editor.submit(), "/quit");
        editor.insert("/sta");
        key(&mut editor, Esc, KeyModifiers::NONE);
        assert!(editor.choices().is_empty());
        assert_eq!(editor.text, "/sta");
        key(&mut editor, Right, KeyModifiers::NONE);
        assert_eq!(editor.text, "/sta");
        editor.insert("t");
        assert_eq!(editor.submit(), "/status");
        editor.insert("/unknown");
        assert_eq!(editor.submit(), "/unknown");
    }

    #[test]
    fn revision_changes_for_text_edits_but_not_navigation() {
        let mut editor = Editor::new(vec!["previous".into()], "draft".into());
        let revision = editor.revision();
        for code in [KeyCode::Left, KeyCode::Right, KeyCode::Home, KeyCode::End] {
            editor.key(KeyEvent::new(code, KeyModifiers::NONE));
            assert_eq!(editor.revision(), revision);
        }
        editor.key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));
        assert_ne!(editor.revision(), revision);
        let revision = editor.revision();
        editor.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(editor.text, "previous");
        assert_ne!(editor.revision(), revision);
        let revision = editor.revision();
        editor.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        editor.insert("prev");
        assert_ne!(editor.revision(), revision);
        let revision = editor.revision();
        editor.key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert_ne!(editor.revision(), revision);
    }

    #[test]
    fn tab_completes_selected_command_without_submitting() {
        use KeyCode::*;
        for (prefix, select_next, expected) in [
            ("/", false, "/model"),
            ("/", true, "/permissions"),
            ("/sta", false, "/status"),
        ] {
            let mut editor = Editor::new(vec![], prefix.into());
            if select_next {
                key(&mut editor, Down, KeyModifiers::NONE);
            }
            assert!(matches!(
                key(&mut editor, Tab, KeyModifiers::NONE),
                Action::Redraw
            ));
            assert_eq!(editor.text, expected);
            assert_eq!(editor.cursor, expected.len());
            key(&mut editor, Tab, KeyModifiers::NONE);
            assert_eq!(editor.text, expected);
            key(&mut editor, Char('_'), KeyModifiers::CONTROL);
            assert_eq!(editor.text, prefix);
            assert_eq!(editor.cursor, prefix.len());
        }
        let mut editor = Editor::new(vec![], "/sta".into());
        key(&mut editor, Tab, KeyModifiers::NONE);
        assert!(matches!(
            key(&mut editor, Enter, KeyModifiers::NONE),
            Action::Submit
        ));
        assert_eq!(editor.submit(), "/status");
        editor.insert("hello");
        key(&mut editor, Tab, KeyModifiers::NONE);
        assert_eq!(editor.text, "hello\t");
    }

    #[test]
    fn shortcuts_clear_delete_kill_yank_undo_and_exit() {
        use KeyCode::*;
        let mut editor = Editor::new(vec![], "one 界界 three".into());
        key(&mut editor, Char('w'), KeyModifiers::CONTROL);
        assert_eq!(editor.text, "one 界界 ");
        key(&mut editor, Char('y'), KeyModifiers::CONTROL);
        assert_eq!(editor.text, "one 界界 three");
        key(&mut editor, Char('a'), KeyModifiers::CONTROL);
        key(&mut editor, Char('d'), KeyModifiers::CONTROL);
        assert_eq!(editor.text, "ne 界界 three");
        key(&mut editor, Char('_'), KeyModifiers::CONTROL);
        assert_eq!(editor.text, "one 界界 three");
        key(&mut editor, Char('c'), KeyModifiers::CONTROL);
        assert_eq!(editor.text, "");
        assert!(matches!(
            key(&mut editor, Char('c'), KeyModifiers::CONTROL),
            Action::Redraw
        ));
        assert!(matches!(
            key(&mut editor, Char('d'), KeyModifiers::CONTROL),
            Action::Eof
        ));
        editor.insert("two lines\n界e\u{301}");
        key(&mut editor, Up, KeyModifiers::NONE);
        assert_eq!(editor.cursor, 1);
        key(&mut editor, Down, KeyModifiers::NONE);
        assert_eq!(editor.cursor, editor.text.len());
    }
}
