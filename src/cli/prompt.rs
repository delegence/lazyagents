use std::collections::BTreeSet;
use std::io::{self, IsTerminal, Write};

use crossterm::cursor;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType};

use crate::error::{Error, Result};

const INTERACTIVE_HINT: &str =
    "↑↓: move • space: toggle • a: all • n: none • enter: done • esc: cancel";
const SINGLE_SELECT_HINT: &str = "↑↓: move • enter: select • esc: cancel";

#[derive(Clone, Debug)]
pub struct PromptConfig {
    pub title: String,
    pub prompt: String,
    pub empty_message: String,
    pub empty_selection_message: Option<String>,
    pub success_message: Option<String>,
    pub actions_hint: Option<String>,
    pub default_select_all: bool,
    pub empty_children_label: String,
}

impl PromptConfig {
    pub fn new(
        title: impl Into<String>,
        prompt: impl Into<String>,
        empty_message: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            prompt: prompt.into(),
            empty_message: empty_message.into(),
            empty_selection_message: None,
            success_message: None,
            actions_hint: Some(INTERACTIVE_HINT.to_string()),
            default_select_all: true,
            empty_children_label: "(no items)".to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TreeSection {
    pub label: String,
    pub items: Vec<TreeItem>,
}

#[derive(Clone, Debug)]
pub struct TreeItem {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct ListItem {
    pub id: String,
    pub label: String,
}

pub fn select_one(config: PromptConfig, items: &[ListItem]) -> Result<String> {
    if items.is_empty() {
        return Err(Error::InvalidInput(config.empty_message));
    }

    if is_interactive() {
        select_one_interactive(&config, items)
    } else {
        select_one_line(&config, items)
    }
}

pub fn select_tree(config: PromptConfig, sections: &[TreeSection]) -> Result<BTreeSet<String>> {
    if sections.iter().all(|section| section.items.is_empty()) {
        return Err(Error::InvalidInput(config.empty_message));
    }

    let mut selected: Vec<Vec<bool>> = sections
        .iter()
        .map(|section| vec![config.default_select_all; section.items.len()])
        .collect();

    if is_interactive() {
        select_tree_interactive(&config, sections, &mut selected)?;
    } else {
        select_tree_line(&config, sections, &mut selected)?;
    }

    let mut chosen = BTreeSet::new();
    for (section_idx, section) in sections.iter().enumerate() {
        for (item_idx, item) in section.items.iter().enumerate() {
            if selected
                .get(section_idx)
                .and_then(|row| row.get(item_idx))
                .copied()
                .unwrap_or(false)
            {
                chosen.insert(item.id.clone());
            }
        }
    }

    if chosen.is_empty() {
        let message = config
            .empty_selection_message
            .unwrap_or_else(|| config.empty_message);
        return Err(Error::InvalidInput(message));
    }

    if let Some(message) = config.success_message {
        println!("{}", message);
    }

    Ok(chosen)
}

pub fn select_list(config: PromptConfig, items: &[ListItem]) -> Result<BTreeSet<String>> {
    if items.is_empty() {
        return Ok(BTreeSet::new());
    }

    let mut selected: Vec<bool> = vec![config.default_select_all; items.len()];

    if is_interactive() {
        select_list_interactive(&config, items, &mut selected)?;
    } else {
        select_list_line(&config, items, &mut selected)?;
    }

    let mut chosen = BTreeSet::new();
    for (idx, item) in items.iter().enumerate() {
        if *selected.get(idx).unwrap_or(&false) {
            chosen.insert(item.id.clone());
        }
    }

    if chosen.is_empty() {
        if let Some(message) = config.empty_selection_message {
            return Err(Error::InvalidInput(message));
        }
    }

    if let Some(message) = config.success_message {
        println!("{}", message);
    }

    Ok(chosen)
}

fn render_tree(config: &PromptConfig, sections: &[TreeSection], _selected: &[Vec<bool>]) {
    println!("{}", config.title);
    if let Some(hint) = &config.actions_hint {
        println!("{}", hint);
    }
    for (section_idx, section) in sections.iter().enumerate() {
        println!("{}. {}", section_idx + 1, section.label);
        if section.items.is_empty() {
            println!("   {}", config.empty_children_label);
            continue;
        }
        for (item_idx, item) in section.items.iter().enumerate() {
            println!("   {}.{}. {}", section_idx + 1, item_idx + 1, item.label);
        }
    }
}

fn render_list(config: &PromptConfig, items: &[ListItem], _selected: &[bool]) {
    println!("{}", config.title);
    if let Some(hint) = &config.actions_hint {
        println!("{}", hint);
    }
    for (idx, item) in items.iter().enumerate() {
        println!("{}. {}", idx + 1, item.label);
    }
}

fn section_state(selected: &[bool]) -> &'static str {
    if selected.is_empty() {
        " "
    } else if selected.iter().all(|item| *item) {
        "x"
    } else if selected.iter().any(|item| *item) {
        "-"
    } else {
        " "
    }
}

fn parse_tree_token(token: &str) -> Option<(usize, Option<usize>)> {
    if let Some((section_part, item_part)) = token.split_once('.') {
        let section_idx = section_part.trim().parse::<usize>().ok()?.saturating_sub(1);
        let item_idx = item_part.trim().parse::<usize>().ok()?.saturating_sub(1);
        Some((section_idx, Some(item_idx)))
    } else {
        let section_idx = token.trim().parse::<usize>().ok()?.saturating_sub(1);
        Some((section_idx, None))
    }
}

pub(crate) fn is_interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

fn select_tree_line(
    config: &PromptConfig,
    sections: &[TreeSection],
    selected: &mut [Vec<bool>],
) -> Result<()> {
    let mut render_config = config.clone();
    render_config.actions_hint = None;
    loop {
        render_tree(&render_config, sections, selected);
        let input = read_input(&config.prompt)?;
        let trimmed = input.trim();
        if trimmed.is_empty() {
            break;
        }
        if trimmed.eq_ignore_ascii_case("all") {
            for row in &mut *selected {
                for item in row {
                    *item = true;
                }
            }
            break;
        }
        clear_tree(selected);
        for token in trimmed.split(|ch: char| ch.is_whitespace() || ch == ',') {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            if let Some((section_idx, item_idx)) = parse_tree_token(token) {
                if section_idx >= sections.len() {
                    continue;
                }
                if let Some(item_idx) = item_idx {
                    select_item_synced(selected, sections, section_idx, item_idx);
                } else {
                    select_section_synced(selected, sections, section_idx);
                }
            }
        }
        break;
    }
    Ok(())
}

fn select_list_line(
    config: &PromptConfig,
    items: &[ListItem],
    selected: &mut [bool],
) -> Result<()> {
    let mut render_config = config.clone();
    render_config.actions_hint = None;
    loop {
        render_list(&render_config, items, selected);
        let input = read_input(&config.prompt)?;
        let trimmed = input.trim();
        if trimmed.is_empty() {
            break;
        }
        if trimmed.eq_ignore_ascii_case("all") {
            for item in &mut *selected {
                *item = true;
            }
            break;
        }

        clear_list(selected);
        for token in trimmed.split(|ch: char| ch.is_whitespace() || ch == ',') {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            if let Ok(index) = token.parse::<usize>() {
                let idx = index.saturating_sub(1);
                if let Some(item) = selected.get_mut(idx) {
                    *item = true;
                }
            }
        }
        break;
    }
    Ok(())
}

fn select_one_line(config: &PromptConfig, items: &[ListItem]) -> Result<String> {
    let mut render_config = config.clone();
    render_config.actions_hint = None;
    render_list(&render_config, items, &[]);

    let input = read_input(&config.prompt)?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        let message = config
            .empty_selection_message
            .clone()
            .unwrap_or_else(|| "no selection provided".to_string());
        return Err(Error::InvalidInput(message));
    }
    if let Ok(index) = trimmed.parse::<usize>() {
        let idx = index.saturating_sub(1);
        if let Some(item) = items.get(idx) {
            return Ok(item.id.clone());
        }
    }
    if let Some(item) = items
        .iter()
        .find(|item| item.id.eq_ignore_ascii_case(trimmed))
    {
        return Ok(item.id.clone());
    }
    Err(Error::InvalidInput(format!(
        "invalid selection '{}'",
        trimmed
    )))
}

fn select_one_interactive(config: &PromptConfig, items: &[ListItem]) -> Result<String> {
    let mut cursor = 0usize;
    let mut stdout = io::stdout();
    let _guard = TerminalGuard::new(&mut stdout)?;

    loop {
        render_single_list_interactive(&mut stdout, config, items, cursor)?;
        let event = event::read().map_err(|err| Error::io("terminal", err))?;
        if let Event::Key(key) = event {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    if cursor > 0 {
                        cursor -= 1;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if cursor + 1 < items.len() {
                        cursor += 1;
                    }
                }
                KeyCode::Enter => return Ok(items[cursor].id.clone()),
                KeyCode::Esc => return Err(Error::InvalidInput("cancelled".to_string())),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(Error::InvalidInput("cancelled".to_string()));
                }
                _ => {}
            }
        }
    }
}

fn select_tree_interactive(
    config: &PromptConfig,
    sections: &[TreeSection],
    selected: &mut [Vec<bool>],
) -> Result<()> {
    let rows = build_tree_rows(sections);
    let Some(mut cursor) = first_selectable_row(&rows) else {
        return Ok(());
    };
    let mut stdout = io::stdout();
    let _guard = TerminalGuard::new(&mut stdout)?;

    loop {
        render_tree_interactive(&mut stdout, config, sections, selected, &rows, cursor)?;
        let event = event::read().map_err(|err| Error::io("terminal", err))?;
        if let Event::Key(key) = event {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    cursor = move_cursor(&rows, cursor, Direction::Up);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    cursor = move_cursor(&rows, cursor, Direction::Down);
                }
                KeyCode::Char(' ') => toggle_tree_row(selected, sections, &rows[cursor]),
                KeyCode::Char('a') | KeyCode::Char('A') => set_all_tree(selected, true),
                KeyCode::Char('n') | KeyCode::Char('N') => set_all_tree(selected, false),
                KeyCode::Enter => break,
                KeyCode::Esc => return Err(Error::InvalidInput("cancelled".to_string())),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(Error::InvalidInput("cancelled".to_string()));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn select_list_interactive(
    config: &PromptConfig,
    items: &[ListItem],
    selected: &mut [bool],
) -> Result<()> {
    let mut cursor = 0usize;
    let mut stdout = io::stdout();
    let _guard = TerminalGuard::new(&mut stdout)?;

    loop {
        render_list_interactive(&mut stdout, config, items, selected, cursor)?;
        let event = event::read().map_err(|err| Error::io("terminal", err))?;
        if let Event::Key(key) = event {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    if cursor > 0 {
                        cursor -= 1;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if cursor + 1 < items.len() {
                        cursor += 1;
                    }
                }
                KeyCode::Char(' ') => {
                    if let Some(item) = selected.get_mut(cursor) {
                        *item = !*item;
                    }
                }
                KeyCode::Char('a') | KeyCode::Char('A') => set_all_list(selected, true),
                KeyCode::Char('n') | KeyCode::Char('N') => set_all_list(selected, false),
                KeyCode::Enter => break,
                KeyCode::Esc => return Err(Error::InvalidInput("cancelled".to_string())),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(Error::InvalidInput("cancelled".to_string()));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn render_tree_interactive(
    stdout: &mut io::Stdout,
    config: &PromptConfig,
    sections: &[TreeSection],
    selected: &[Vec<bool>],
    rows: &[TreeRow],
    cursor: usize,
) -> Result<()> {
    crossterm::execute!(stdout, cursor::MoveTo(0, 0), Clear(ClearType::All))
        .map_err(|err| Error::io("terminal", err))?;
    let mut output = String::new();
    push_line(&mut output, &config.title);
    let (selected_count, total_count) = tree_counts(selected);
    push_line(
        &mut output,
        &format!("Selected {}/{}", selected_count, total_count),
    );
    push_line(&mut output, "");
    for (row_idx, row) in rows.iter().enumerate() {
        let cursor_mark = if row_idx == cursor { ">" } else { " " };
        match *row {
            TreeRow::Section(section_idx) => {
                let state = section_state(
                    selected
                        .get(section_idx)
                        .map(|row| row.as_slice())
                        .unwrap_or(&[]),
                );
                let label = &sections[section_idx].label;
                push_line(
                    &mut output,
                    &format!("{} [{}] {}", cursor_mark, state, label),
                );
            }
            TreeRow::Item {
                section_idx,
                item_idx,
            } => {
                let is_selected = selected
                    .get(section_idx)
                    .and_then(|row| row.get(item_idx))
                    .copied()
                    .unwrap_or(false);
                let mark = if is_selected { "x" } else { " " };
                let label = &sections[section_idx].items[item_idx].label;
                push_line(
                    &mut output,
                    &format!("{}   [{}] {}", cursor_mark, mark, label),
                );
            }
            TreeRow::Empty => {
                push_line(
                    &mut output,
                    &format!("{}   {}", cursor_mark, config.empty_children_label),
                );
            }
        }
    }
    push_line(&mut output, "");
    push_line(&mut output, INTERACTIVE_HINT);
    stdout
        .write_all(output.as_bytes())
        .map_err(|err| Error::io("stdout", err))?;
    stdout.flush().map_err(|err| Error::io("stdout", err))?;
    Ok(())
}

fn render_list_interactive(
    stdout: &mut io::Stdout,
    config: &PromptConfig,
    items: &[ListItem],
    selected: &[bool],
    cursor: usize,
) -> Result<()> {
    crossterm::execute!(stdout, cursor::MoveTo(0, 0), Clear(ClearType::All))
        .map_err(|err| Error::io("terminal", err))?;
    let mut output = String::new();
    push_line(&mut output, &config.title);
    let selected_count = selected.iter().filter(|item| **item).count();
    push_line(
        &mut output,
        &format!("Selected {}/{}", selected_count, items.len()),
    );
    push_line(&mut output, "");
    for (idx, item) in items.iter().enumerate() {
        let cursor_mark = if idx == cursor { ">" } else { " " };
        let mark = if *selected.get(idx).unwrap_or(&false) {
            "x"
        } else {
            " "
        };
        push_line(
            &mut output,
            &format!("{} [{}] {}", cursor_mark, mark, item.label),
        );
    }
    push_line(&mut output, "");
    push_line(&mut output, INTERACTIVE_HINT);
    stdout
        .write_all(output.as_bytes())
        .map_err(|err| Error::io("stdout", err))?;
    stdout.flush().map_err(|err| Error::io("stdout", err))?;
    Ok(())
}

fn render_single_list_interactive(
    stdout: &mut io::Stdout,
    config: &PromptConfig,
    items: &[ListItem],
    cursor: usize,
) -> Result<()> {
    crossterm::execute!(stdout, cursor::MoveTo(0, 0), Clear(ClearType::All))
        .map_err(|err| Error::io("terminal", err))?;
    let mut output = String::new();
    push_line(&mut output, &config.title);
    push_line(&mut output, "");
    for (idx, item) in items.iter().enumerate() {
        let cursor_mark = if idx == cursor { ">" } else { " " };
        push_line(&mut output, &format!("{} {}", cursor_mark, item.label));
    }
    push_line(&mut output, "");
    push_line(&mut output, SINGLE_SELECT_HINT);
    stdout
        .write_all(output.as_bytes())
        .map_err(|err| Error::io("stdout", err))?;
    stdout.flush().map_err(|err| Error::io("stdout", err))?;
    Ok(())
}

fn tree_counts(selected: &[Vec<bool>]) -> (usize, usize) {
    let mut selected_count = 0;
    let mut total_count = 0;
    for row in selected {
        total_count += row.len();
        selected_count += row.iter().filter(|item| **item).count();
    }
    (selected_count, total_count)
}

fn set_all_tree(selected: &mut [Vec<bool>], value: bool) {
    for row in selected {
        for item in row {
            *item = value;
        }
    }
}

fn set_all_list(selected: &mut [bool], value: bool) {
    for item in selected {
        *item = value;
    }
}

fn toggle_tree_row(selected: &mut [Vec<bool>], sections: &[TreeSection], row: &TreeRow) {
    match *row {
        TreeRow::Section(section_idx) => toggle_section_synced(selected, sections, section_idx),
        TreeRow::Item {
            section_idx,
            item_idx,
        } => toggle_item_synced(selected, sections, section_idx, item_idx),
        TreeRow::Empty => {}
    }
}

#[derive(Clone, Copy, Debug)]
enum TreeRow {
    Section(usize),
    Item { section_idx: usize, item_idx: usize },
    Empty,
}

fn build_tree_rows(sections: &[TreeSection]) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    for (section_idx, section) in sections.iter().enumerate() {
        rows.push(TreeRow::Section(section_idx));
        if section.items.is_empty() {
            rows.push(TreeRow::Empty);
        } else {
            for item_idx in 0..section.items.len() {
                rows.push(TreeRow::Item {
                    section_idx,
                    item_idx,
                });
            }
        }
    }
    rows
}

fn first_selectable_row(rows: &[TreeRow]) -> Option<usize> {
    rows.iter()
        .position(|row| matches!(row, TreeRow::Section(_) | TreeRow::Item { .. }))
}

enum Direction {
    Up,
    Down,
}

fn move_cursor(rows: &[TreeRow], cursor: usize, direction: Direction) -> usize {
    let mut idx = cursor as isize;
    loop {
        idx = match direction {
            Direction::Up => idx - 1,
            Direction::Down => idx + 1,
        };
        if idx < 0 || idx >= rows.len() as isize {
            return cursor;
        }
        let row = &rows[idx as usize];
        if matches!(row, TreeRow::Section(_) | TreeRow::Item { .. }) {
            return idx as usize;
        }
    }
}

struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    fn new(stdout: &mut io::Stdout) -> Result<Self> {
        enable_raw_mode().map_err(|err| Error::io("terminal", err))?;
        if let Err(err) = crossterm::execute!(stdout, cursor::Hide, cursor::MoveTo(0, 0)) {
            let _ = disable_raw_mode();
            let _ = crossterm::execute!(stdout, cursor::Show);
            return Err(Error::io("terminal", err));
        }
        Ok(Self { active: true })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut stdout = io::stdout();
        let _ = crossterm::execute!(
            stdout,
            cursor::MoveTo(0, 0),
            Clear(ClearType::All),
            cursor::Show
        );
        let _ = disable_raw_mode();
    }
}

fn push_line(output: &mut String, line: &str) {
    output.push_str(line);
    output.push_str("\r\n");
}

fn set_item_by_id(selected: &mut [Vec<bool>], sections: &[TreeSection], id: &str, value: bool) {
    for (section_idx, section) in sections.iter().enumerate() {
        for (item_idx, item) in section.items.iter().enumerate() {
            if item.id == id {
                if let Some(row) = selected.get_mut(section_idx) {
                    if let Some(slot) = row.get_mut(item_idx) {
                        *slot = value;
                    }
                }
            }
        }
    }
}

fn toggle_item_synced(
    selected: &mut [Vec<bool>],
    sections: &[TreeSection],
    section_idx: usize,
    item_idx: usize,
) {
    let Some(section) = sections.get(section_idx) else {
        return;
    };
    let Some(item) = section.items.get(item_idx) else {
        return;
    };
    let current = selected
        .get(section_idx)
        .and_then(|row| row.get(item_idx))
        .copied()
        .unwrap_or(false);
    set_item_by_id(selected, sections, &item.id, !current);
}

fn toggle_section_synced(selected: &mut [Vec<bool>], sections: &[TreeSection], section_idx: usize) {
    let Some(section) = sections.get(section_idx) else {
        return;
    };
    let target = selected
        .get(section_idx)
        .map(|row| !row.iter().all(|item| *item))
        .unwrap_or(true);
    for item in &section.items {
        set_item_by_id(selected, sections, &item.id, target);
    }
}

fn clear_tree(selected: &mut [Vec<bool>]) {
    for row in selected {
        for item in row {
            *item = false;
        }
    }
}

fn clear_list(selected: &mut [bool]) {
    for item in selected {
        *item = false;
    }
}

fn select_item_synced(
    selected: &mut [Vec<bool>],
    sections: &[TreeSection],
    section_idx: usize,
    item_idx: usize,
) {
    let Some(section) = sections.get(section_idx) else {
        return;
    };
    let Some(item) = section.items.get(item_idx) else {
        return;
    };
    set_item_by_id(selected, sections, &item.id, true);
}

fn select_section_synced(selected: &mut [Vec<bool>], sections: &[TreeSection], section_idx: usize) {
    let Some(section) = sections.get(section_idx) else {
        return;
    };
    for item in &section.items {
        set_item_by_id(selected, sections, &item.id, true);
    }
}

pub(crate) fn read_input(prompt: &str) -> Result<String> {
    let mut input = String::new();
    print!("{}", prompt);
    io::stdout()
        .flush()
        .map_err(|err| Error::io("stdout", err))?;
    io::stdin()
        .read_line(&mut input)
        .map_err(|err| Error::io("stdin", err))?;
    Ok(input)
}

pub(crate) fn confirm_yes_no(prompt: &str, default_yes: bool) -> Result<bool> {
    loop {
        let input = read_input(prompt)?;
        let trimmed = input.trim().to_lowercase();
        if trimmed.is_empty() {
            return Ok(default_yes);
        }
        if trimmed == "y" || trimmed == "yes" {
            return Ok(true);
        }
        if trimmed == "n" || trimmed == "no" {
            return Ok(false);
        }
    }
}
