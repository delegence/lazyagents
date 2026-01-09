use std::fs;
use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Terminal;

use crate::cli::doctor;
use crate::core::{self, AgentScope, ConfigFile, Profile, ProfileDraft};
use crate::error::{Error, Result};
use crate::harness;

pub fn run() -> Result<()> {
    doctor::sync()?;
    let config = ConfigFile::load_or_create()?;
    let mut app = App::new(config);
    app.reset_profile_selection();

    enable_raw_mode().map_err(|err| Error::io("terminal", err))?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)
        .map_err(|err| Error::io("terminal", err))?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(|err| Error::io("terminal", err))?;

    let result = app.run(&mut terminal);

    disable_raw_mode().map_err(|err| Error::io("terminal", err))?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )
    .map_err(|err| Error::io("terminal", err))?;
    terminal
        .show_cursor()
        .map_err(|err| Error::io("terminal", err))?;

    result
}

enum Mode {
    View,
    CreateId,
    CreateAgents,
    ConfirmDelete,
}

struct App {
    config: ConfigFile,
    selected: usize,
    selected_agent: usize,
    mode: Mode,
    input: String,
    status: Option<String>,
    create_agent_index: usize,
    create_agent_selected: Vec<bool>,
}

impl App {
    fn new(config: ConfigFile) -> Self {
        Self {
            config,
            selected: 0,
            selected_agent: 0,
            mode: Mode::View,
            input: String::new(),
            status: None,
            create_agent_index: 0,
            create_agent_selected: Vec::new(),
        }
    }

    fn run<B: ratatui::backend::Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<()> {
        loop {
            terminal
                .draw(|frame| self.render(frame))
                .map_err(|err| Error::io("terminal", err))?;

            if event::poll(Duration::from_millis(200)).map_err(|err| Error::io("terminal", err))? {
                if let Event::Key(key) = event::read().map_err(|err| Error::io("terminal", err))? {
                    if key.kind == KeyEventKind::Press {
                        if self.handle_key(key)? {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        match self.mode {
            Mode::View => self.handle_view_key(key),
            Mode::CreateId => self.handle_create_id_key(key),
            Mode::CreateAgents => self.handle_create_agents_key(key),
            Mode::ConfirmDelete => self.handle_confirm_delete_key(key),
        }
    }

    fn handle_view_key(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Up => self.select_previous(),
            KeyCode::Down => self.select_next(),
            KeyCode::Tab => self.select_next_agent(),
            KeyCode::Char('n') => self.start_create(),
            KeyCode::Char('d') => self.start_delete(),
            KeyCode::Char('s') | KeyCode::Enter => self.switch_selected(),
            _ => {}
        }
        Ok(false)
    }

    fn handle_create_id_key(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc => {
                self.cancel_modal();
            }
            KeyCode::Enter => {
                if self.input.trim().is_empty() {
                    self.status = Some("profile id cannot be empty".to_string());
                } else {
                    self.start_agent_select();
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(ch) => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    return Ok(false);
                }
                if !ch.is_control() {
                    self.input.push(ch);
                }
            }
            _ => {}
        }
        Ok(false)
    }

    fn handle_create_agents_key(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc => {
                self.cancel_modal();
            }
            KeyCode::Up => {
                if self.create_agent_index > 0 {
                    self.create_agent_index -= 1;
                }
            }
            KeyCode::Down => {
                if self.create_agent_index + 1 < self.create_agent_selected.len() {
                    self.create_agent_index += 1;
                }
            }
            KeyCode::Char(' ') => self.toggle_agent(),
            KeyCode::Enter => self.create_profile(),
            _ => {}
        }
        Ok(false)
    }

    fn handle_confirm_delete_key(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Char('y') => self.delete_selected(),
            KeyCode::Char('n') | KeyCode::Esc => self.cancel_modal(),
            _ => {}
        }
        Ok(false)
    }

    fn select_previous(&mut self) {
        let count = self.filtered_profile_indexes().len();
        if count == 0 {
            self.selected = 0;
            return;
        }
        if self.selected == 0 {
            self.selected = count.saturating_sub(1);
        } else {
            self.selected -= 1;
        }
    }

    fn select_next(&mut self) {
        let count = self.filtered_profile_indexes().len();
        if count == 0 {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected + 1) % count;
    }

    fn select_next_agent(&mut self) {
        if self.config.agents.is_empty() {
            self.selected_agent = 0;
            return;
        }
        self.selected_agent = (self.selected_agent + 1) % self.config.agents.len();
        self.reset_profile_selection();
    }

    fn reset_profile_selection(&mut self) {
        let active = self
            .selected_agent_id()
            .and_then(|agent_id| self.config.active_profiles.get(agent_id));
        let profiles = self.filtered_profile_indexes();
        if let Some(active_id) = active {
            if let Some(pos) = profiles
                .iter()
                .position(|idx| self.config.profiles[*idx].id == *active_id)
            {
                self.selected = pos;
                return;
            }
        }
        self.selected = 0;
    }

    fn selected_agent_id(&self) -> Option<&str> {
        self.config
            .agents
            .get(self.selected_agent)
            .map(|agent| agent.id.as_str())
    }

    fn filtered_profile_indexes(&self) -> Vec<usize> {
        let Some(agent_id) = self.selected_agent_id() else {
            return Vec::new();
        };
        self.config
            .profiles
            .iter()
            .enumerate()
            .filter(|(_, profile)| profile.agents.iter().any(|agent| agent == agent_id))
            .map(|(idx, _)| idx)
            .collect()
    }

    fn selected_profile(&self) -> Option<&Profile> {
        let profiles = self.filtered_profile_indexes();
        profiles
            .get(self.selected)
            .and_then(|idx| self.config.profiles.get(*idx))
    }

    fn start_create(&mut self) {
        self.mode = Mode::CreateId;
        self.input.clear();
        self.status = None;
    }

    fn start_agent_select(&mut self) {
        self.mode = Mode::CreateAgents;
        self.create_agent_index = 0;
        self.create_agent_selected = vec![false; self.config.agents.len()];
    }

    fn toggle_agent(&mut self) {
        if let Some(selected) = self.create_agent_selected.get_mut(self.create_agent_index) {
            *selected = !*selected;
        }
    }

    fn create_profile(&mut self) {
        let id = self.input.trim().to_string();
        let mut draft = ProfileDraft::minimal(id.clone());
        for (idx, agent) in self.config.agents.iter().enumerate() {
            if self
                .create_agent_selected
                .get(idx)
                .copied()
                .unwrap_or(false)
            {
                draft.agents.push(agent.id.clone());
            }
        }

        match core::create_profile(&mut self.config, draft) {
            Ok(()) => {
                if let Err(err) = self.config.save() {
                    self.status = Some(err.to_string());
                    self.mode = Mode::View;
                    return;
                }
                let profiles = self.filtered_profile_indexes();
                if let Some(pos) = profiles
                    .iter()
                    .position(|idx| self.config.profiles[*idx].id == id)
                {
                    self.selected = pos;
                } else {
                    self.selected = 0;
                }
                self.status = Some(format!("created profile '{}'", id));
                self.mode = Mode::View;
            }
            Err(err) => {
                self.status = Some(err.to_string());
                self.mode = Mode::CreateId;
            }
        }
    }

    fn start_delete(&mut self) {
        if self.filtered_profile_indexes().is_empty() {
            self.status = Some("no profiles to delete for agent".to_string());
            return;
        }
        self.mode = Mode::ConfirmDelete;
    }

    fn delete_selected(&mut self) {
        let Some(profile_id) = self.selected_profile_id() else {
            self.status = Some("no profile selected".to_string());
            self.mode = Mode::View;
            return;
        };

        match core::remove_profile(&mut self.config, &profile_id) {
            Ok(()) => {
                if let Err(err) = self.config.save() {
                    self.status = Some(err.to_string());
                    self.mode = Mode::View;
                    return;
                }
                let count = self.filtered_profile_indexes().len();
                if self.selected >= count {
                    self.selected = count.saturating_sub(1);
                }
                self.status = Some(format!("deleted profile '{}'", profile_id));
            }
            Err(err) => {
                self.status = Some(err.to_string());
            }
        }

        self.mode = Mode::View;
    }

    fn switch_selected(&mut self) {
        let Some(profile_id) = self.selected_profile_id() else {
            self.status = Some("no profile selected".to_string());
            return;
        };
        let Some(agent_id) = self.selected_agent_id().map(|id| id.to_string()) else {
            self.status = Some("no agent selected".to_string());
            return;
        };

        let result = core::switch_profile(
            &mut self.config,
            &profile_id,
            AgentScope::OnlyAgent(agent_id.clone()),
        );
        let report = match result {
            Ok(report) => report,
            Err(err) => {
                self.status = Some(err.to_string());
                return;
            }
        };

        let Some(profile) = self
            .config
            .profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .cloned()
        else {
            self.status = Some("profile missing after switch".to_string());
            return;
        };

        if let Err(err) = harness::apply_profile_for_agent(&self.config, &profile, &agent_id) {
            self.status = Some(err.to_string());
            return;
        }

        if let Err(err) = self.config.save() {
            self.status = Some(err.to_string());
            return;
        }

        if report.warnings.is_empty() {
            self.status = Some(format!("switched to '{}' for {}", profile_id, agent_id));
        } else {
            let warning = report.warnings.join("; ");
            self.status = Some(format!(
                "switched to '{}' for {}: {}",
                profile_id, agent_id, warning
            ));
        }
    }

    fn cancel_modal(&mut self) {
        self.mode = Mode::View;
    }

    fn selected_profile_id(&self) -> Option<String> {
        self.selected_profile().map(|profile| profile.id.clone())
    }

    fn render(&self, frame: &mut ratatui::Frame<'_>) {
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(10),
                Constraint::Length(2),
            ])
            .split(frame.area());

        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(32), Constraint::Min(20)])
            .split(vertical[1]);

        self.render_agents(frame, vertical[0]);
        self.render_profiles(frame, layout[0]);
        self.render_profile_detail(frame, layout[1]);
        self.render_footer(frame, vertical[2]);

        match self.mode {
            Mode::CreateId => self.render_create_id(frame),
            Mode::CreateAgents => self.render_create_agents(frame),
            Mode::ConfirmDelete => self.render_confirm_delete(frame),
            Mode::View => {}
        }
    }

    fn render_agents(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let mut spans = Vec::new();
        for (idx, agent) in self.config.agents.iter().enumerate() {
            let mut label = agent_display_name(&agent.id).to_string();
            let mut style = Style::default();
            if !agent.installed {
                style = style.fg(Color::DarkGray);
            }
            if idx == self.selected_agent {
                label = format!("[{}]", label);
                style = style.add_modifier(Modifier::BOLD);
            }
            spans.push(Span::styled(label, style));
            spans.push(Span::raw("  "));
        }

        let block = Block::default().title("Agents").borders(Borders::ALL);
        frame.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
    }

    fn render_profiles(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let block = Block::default().title("Profiles").borders(Borders::ALL);
        let profiles = self.filtered_profile_indexes();
        if profiles.is_empty() {
            frame.render_widget(Paragraph::new("No profiles for agent").block(block), area);
            return;
        }

        let active_profile = self
            .selected_agent_id()
            .and_then(|agent_id| self.config.active_profiles.get(agent_id));
        let items: Vec<ListItem> = profiles
            .iter()
            .filter_map(|idx| self.config.profiles.get(*idx))
            .map(|profile| {
                let label = if Some(&profile.id) == active_profile {
                    format!("{} (active)", profile.id)
                } else {
                    profile.id.clone()
                };
                ListItem::new(Line::from(Span::styled(
                    label,
                    Style::default().add_modifier(Modifier::BOLD),
                )))
            })
            .collect();

        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▸ ");

        frame.render_stateful_widget(list, area, &mut self.list_state());
    }

    fn render_profile_detail(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let block = Block::default().title("Details").borders(Borders::ALL);
        let content = if let Some(profile) = self.selected_profile() {
            let mut lines = Vec::new();
            lines.push(Line::from(Span::styled(
                profile.id.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )));

            let model = self
                .selected_agent_id()
                .and_then(|agent_id| profile.models.get(agent_id))
                .cloned()
                .unwrap_or_else(|| "default".to_string());
            lines.push(Line::from(format!("-Model: {}", model)));
            lines.push(Line::from(format!(
                "-Rules: {}",
                rules_first_line(&profile.id)
            )));

            lines.push(Line::from("-Commands:"));
            if profile.commands.is_empty() {
                lines.push(Line::from("  <not detected>"));
            } else {
                for command in &profile.commands {
                    lines.push(Line::from(format!("  --{}", command)));
                }
            }

            lines.push(Line::from("-Skills:"));
            if profile.skills.is_empty() {
                lines.push(Line::from("  <not detected>"));
            } else {
                for skill in &profile.skills {
                    lines.push(Line::from(format!("  --{}", skill)));
                }
            }

            lines.push(Line::from("-MCP servers:"));
            if profile.mcps.is_empty() {
                lines.push(Line::from("  <not detected>"));
            } else {
                for mcp in &profile.mcps {
                    lines.push(Line::from(format!("  --{}", mcp)));
                }
            }

            Paragraph::new(lines)
        } else {
            Paragraph::new("Select a profile")
        };

        frame.render_widget(content.block(block), area);
    }

    fn render_footer(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let help = match self.mode {
            Mode::View => {
                "↑↓: move • tab: agent • n: new profile • d: delete profile • s: switch to profile • q: quit"
            }
            Mode::CreateId => "enter: continue • esc: cancel",
            Mode::CreateAgents => "space: toggle • enter: create • esc: cancel",
            Mode::ConfirmDelete => "y: delete • n/esc: cancel",
        };

        let status = self.status.as_deref().unwrap_or("");

        let lines = vec![Line::from(Span::raw(help)), Line::from(Span::raw(status))];

        frame.render_widget(Paragraph::new(lines), area);
    }

    fn render_create_id(&self, frame: &mut ratatui::Frame<'_>) {
        let width = popup_width(frame.area(), &["Enter profile id:", self.input.as_str()]);
        let area = centered_rect(width, popup_height(frame.area(), 2), frame.area());
        let block = Block::default().title("New Profile").borders(Borders::ALL);
        let text = vec![
            Line::from("Enter profile id:"),
            Line::from(Span::styled(
                self.input.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )),
        ];

        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(text)
                .block(block)
                .alignment(Alignment::Left)
                .wrap(Wrap { trim: true }),
            area,
        );
    }

    fn render_create_agents(&self, frame: &mut ratatui::Frame<'_>) {
        let item_lines: Vec<String> = self
            .config
            .agents
            .iter()
            .enumerate()
            .map(|(idx, agent)| {
                let checked = self
                    .create_agent_selected
                    .get(idx)
                    .copied()
                    .unwrap_or(false);
                let prefix = if checked { "[x]" } else { "[ ]" };
                format!("{} {}", prefix, agent.id)
            })
            .collect();
        let width = popup_width(frame.area(), &item_lines);
        let height = popup_height(frame.area(), item_lines.len().max(1));
        let area = centered_rect(width, height, frame.area());
        let block = Block::default()
            .title("Select Agents")
            .borders(Borders::ALL);

        let items: Vec<ListItem> = item_lines
            .iter()
            .map(|line| ListItem::new(Line::from(line.as_str())))
            .collect();

        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▸ ");

        frame.render_widget(Clear, area);
        frame.render_stateful_widget(list, area, &mut self.agent_list_state());
    }

    fn render_confirm_delete(&self, frame: &mut ratatui::Frame<'_>) {
        let profile = self
            .selected_profile_id()
            .unwrap_or_else(|| "(none)".to_string());
        let line = format!("Delete profile '{}' ?", profile);
        let width = popup_width(frame.area(), &[line.as_str()]);
        let area = centered_rect(width, popup_height(frame.area(), 1), frame.area());
        let block = Block::default()
            .title("Delete Profile")
            .borders(Borders::ALL);
        let text = vec![Line::from(line)];

        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new(text).block(block), area);
    }

    fn list_state(&self) -> ratatui::widgets::ListState {
        let mut state = ratatui::widgets::ListState::default();
        if !self.filtered_profile_indexes().is_empty() {
            state.select(Some(self.selected));
        }
        state
    }

    fn agent_list_state(&self) -> ratatui::widgets::ListState {
        let mut state = ratatui::widgets::ListState::default();
        if !self.config.agents.is_empty() {
            state.select(Some(self.create_agent_index));
        }
        state
    }
}

fn rules_first_line(profile_id: &str) -> String {
    let path = match core::rules_profile_dir(profile_id) {
        Ok(dir) => dir.join("AGENTS.md"),
        Err(_) => return "not detected".to_string(),
    };

    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(_) => return "not detected".to_string(),
    };
    let line = contents.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        "empty".to_string()
    } else {
        line.to_string()
    }
}

fn agent_display_name(id: &str) -> &str {
    match id {
        "codex" => "Codex",
        "claude" => "Claude",
        "opencode" => "OpenCode",
        _ => id,
    }
}

fn centered_rect(width: u16, height: u16, rect: Rect) -> Rect {
    let width = width.min(rect.width.saturating_sub(2)).max(10);
    let height = height.min(rect.height.saturating_sub(2)).max(5);
    let x = rect.x + rect.width.saturating_sub(width) / 2;
    let y = rect.y + rect.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width,
        height,
    }
}

fn popup_width<T: AsRef<str>>(area: Rect, lines: &[T]) -> u16 {
    let mut max_len = lines
        .iter()
        .map(|line| line.as_ref().len())
        .max()
        .unwrap_or(0);
    if max_len == 0 {
        max_len = 10;
    }
    let width = (max_len + 4) as u16;
    width.min(area.width.saturating_sub(2)).max(20)
}

fn popup_height(area: Rect, line_count: usize) -> u16 {
    let height = (line_count + 4) as u16;
    height.min(area.height.saturating_sub(2)).max(4)
}
