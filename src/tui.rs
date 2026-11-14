use std::collections::BTreeSet;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Paragraph},
};

use crate::model::{Entry, EntryId, Snapshot, Status, display_text};

mod list;
mod presentation;
use list::{Group, GroupBy, GroupKey, groups, matching_indices};
use presentation::{
    COLUMN_LABELS, clip_text, columns, compact_line, home_path, measure_columns, shortcuts,
    status_color, status_text, wrap_details,
};

#[derive(Debug)]
pub enum Action {
    Quit,
    Navigate(Box<Entry>),
}

struct HitRegion {
    area: Rect,
    entry: Entry,
}

struct GroupHit {
    area: Rect,
    key: GroupKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ListRow {
    Agent(usize),
    Header {
        key: GroupKey,
        label: String,
        first: usize,
        count: usize,
        collapsed: bool,
    },
}

/// View and input state only. The caller supplies snapshots and runs navigation.
#[derive(Default)]
pub struct App {
    pub snapshot: Snapshot,
    pub message: String,
    pub connected: bool,
    pub navigating: bool,
    pub client: Option<String>,
    pub diagnostic: String,
    group_by: GroupBy,
    collapsed: BTreeSet<GroupKey>,
    query: String,
    filter_edit: Option<String>,
    selected: Option<EntryId>,
    selection_missing: bool,
    received_snapshot: bool,
    scroll: usize,
    reveal_selection: bool,
    details: bool,
    help: bool,
    details_scroll: usize,
    details_limit: usize,
    viewport: Rect,
    hits: Vec<HitRegion>,
    group_hits: Vec<GroupHit>,
}

impl App {
    pub fn replace_snapshot(&mut self, mut snapshot: Snapshot) {
        let old_row = self.selected_row();
        self.group_by.sort(&mut snapshot);
        self.clear_hits();
        self.snapshot = snapshot;
        let keys: BTreeSet<_> = self
            .snapshot
            .entries
            .iter()
            .flat_map(|entry| {
                [
                    GroupBy::Agent,
                    GroupBy::Status,
                    GroupBy::Cwd,
                    GroupBy::Session,
                ]
                .into_iter()
                .filter_map(|mode| mode.key(entry))
            })
            .collect();
        self.collapsed.retain(|key| keys.contains(key));
        if self.selection_missing || (self.selected.is_some() && self.selected_index().is_none()) {
            self.selection_removed();
        } else if !self.received_snapshot {
            self.selected = self.snapshot.entries.first().map(|entry| entry.id.clone());
        }
        self.reveal_selection |= !self.received_snapshot || old_row != self.selected_row();
        self.received_snapshot = true;
    }

    fn clear_hits(&mut self) {
        self.hits.clear();
        self.group_hits.clear();
    }

    pub fn discard_redraw_click(&mut self) {
        if !self.details && !self.help {
            self.message = "List redrawn; click again to select the displayed row".into();
        }
    }

    pub fn handle_event(&mut self, event: Event) -> Option<Action> {
        if let Event::Key(key) = &event {
            if key.kind == KeyEventKind::Release {
                return None;
            }
            if matches!(key.code, KeyCode::Char('c' | 'C'))
                && key.modifiers.contains(KeyModifiers::CONTROL)
            {
                return Some(Action::Quit);
            }
            if self.filter_edit.is_some() {
                match key.code {
                    KeyCode::Esc => {
                        self.query = self.filter_edit.take().unwrap_or_default();
                        self.filter_changed();
                    }
                    KeyCode::Enter => {
                        self.filter_edit = None;
                        self.clear_hits();
                    }
                    KeyCode::Backspace => {
                        let length = ratatui::text::Span::raw(self.query.as_str())
                            .styled_graphemes(Style::default())
                            .last()
                            .map_or(0, |g| g.symbol.len());
                        self.query.truncate(self.query.len().saturating_sub(length));
                        self.filter_changed();
                    }
                    KeyCode::Char(c)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        if self.query.chars().count() < 256 && !c.is_control() {
                            self.query.push(c);
                            self.filter_changed();
                        }
                    }
                    KeyCode::Up | KeyCode::Down => {
                        self.clear_hits();
                        self.move_selection(key.code == KeyCode::Down);
                    }
                    _ => {}
                }
                return None;
            }
            if self.details || self.help {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => self.close_overlay(),
                    KeyCode::Char('w') if self.details => self.close_overlay(),
                    KeyCode::Char('?') if self.help => self.close_overlay(),
                    KeyCode::Up | KeyCode::Down => {
                        self.scroll_details(key.code == KeyCode::Down, 1)
                    }
                    KeyCode::PageUp | KeyCode::PageDown => self.scroll_details(
                        key.code == KeyCode::PageDown,
                        usize::from(self.viewport.height).max(1),
                    ),
                    _ => {}
                }
                return None;
            }
            let mutating = matches!(
                key.code,
                KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Enter
                    | KeyCode::Char('j' | 'k' | 'g' | 'G' | 'a' | 'A' | '/' | '?' | 'w')
            ) || (self.group_by != GroupBy::None
                && matches!(key.code, KeyCode::Tab | KeyCode::BackTab));
            if mutating {
                self.clear_hits();
            }
            return match key.code {
                KeyCode::Char('q') => Some(Action::Quit),
                KeyCode::Esc if !self.query.is_empty() => {
                    self.query.clear();
                    self.filter_changed();
                    None
                }
                KeyCode::Esc => Some(Action::Quit),
                KeyCode::Up | KeyCode::Char('k') => {
                    self.move_selection(false);
                    None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.move_selection(true);
                    None
                }
                KeyCode::Char('g' | 'G') => {
                    self.group_by = if key.code == KeyCode::Char('G') {
                        self.group_by.previous()
                    } else {
                        self.group_by.next()
                    };
                    self.group_by.sort(&mut self.snapshot);
                    self.reveal_selection = true;
                    None
                }
                KeyCode::Tab | KeyCode::BackTab => {
                    self.move_group(key.code == KeyCode::Tab);
                    None
                }
                KeyCode::Left | KeyCode::Right => {
                    self.fold_selected(key.code == KeyCode::Left);
                    None
                }
                KeyCode::Char('a' | 'A') => {
                    self.move_attention(key.code == KeyCode::Char('a'));
                    None
                }
                KeyCode::Char('/') => {
                    self.filter_edit = Some(self.query.clone());
                    None
                }
                KeyCode::Char('?') => {
                    self.help = true;
                    self.reset_overlay();
                    None
                }
                KeyCode::Char('w') => {
                    self.toggle_details();
                    None
                }
                KeyCode::Enter => self.navigate_selected(),
                _ => None,
            };
        }
        if let Event::Paste(value) = &event {
            if self.filter_edit.is_some() {
                let remaining = 256usize.saturating_sub(self.query.chars().count());
                let value: String = value
                    .chars()
                    .map(|c| if c.is_whitespace() { ' ' } else { c })
                    .collect();
                self.query
                    .extend(display_text(&value).chars().take(remaining));
                self.filter_changed();
            }
            return None;
        }
        match event {
            Event::Mouse(mouse) if self.details || self.help => {
                if contains(self.viewport, mouse.column, mouse.row)
                    && matches!(
                        mouse.kind,
                        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                    )
                {
                    self.scroll_details(mouse.kind == MouseEventKind::ScrollDown, 3);
                }
                None
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let group = self
                        .group_hits
                        .iter()
                        .find(|hit| contains(hit.area, mouse.column, mouse.row))
                        .map(|hit| hit.key.clone());
                    let clicked = self
                        .hits
                        .iter()
                        .find(|hit| contains(hit.area, mouse.column, mouse.row))
                        .map(|hit| hit.entry.clone());
                    self.clear_hits();
                    if let Some(key) = group {
                        if self.current_groups().iter().any(|group| group.key == key) {
                            self.toggle_group(key);
                        }
                        return None;
                    }
                    let entry = clicked?;
                    if !self.snapshot.entries.contains(&entry) {
                        self.message =
                            "The list changed. Draw the list again before clicking.".into();
                        return None;
                    }
                    self.selected = Some(entry.id);
                    self.selection_missing = false;
                    self.filter_edit = None;
                    self.message.clear();
                    self.navigate_selected()
                }
                MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                    if contains(self.viewport, mouse.column, mouse.row) =>
                {
                    self.clear_hits();
                    let limit = self
                        .list_rows()
                        .len()
                        .saturating_sub(usize::from(self.viewport.height));
                    self.scroll = if mouse.kind == MouseEventKind::ScrollDown {
                        self.scroll.saturating_add(3).min(limit)
                    } else {
                        self.scroll.saturating_sub(3).min(limit)
                    };
                    self.reveal_selection = false;
                    None
                }
                _ => None,
            },
            Event::Resize(_, _) => {
                self.clear_hits();
                self.viewport = Rect::default();
                if !self.details && !self.help {
                    self.reveal_selection = true;
                }
                None
            }
            _ => None,
        }
    }

    fn filter_changed(&mut self) {
        self.clear_hits();
        self.scroll = 0;
        self.reveal_selection = true;
    }

    fn current_groups(&self) -> Vec<Group> {
        groups(&self.snapshot, self.group_by, &self.query)
    }

    fn list_rows(&self) -> Vec<ListRow> {
        let groups = self.current_groups();
        if groups.len() < 2 {
            return matching_indices(&self.snapshot, &self.query)
                .into_iter()
                .map(ListRow::Agent)
                .collect();
        }
        let mut rows = Vec::new();
        for group in groups {
            let collapsed = self.collapsed.contains(&group.key);
            rows.push(ListRow::Header {
                key: group.key,
                label: group.label,
                first: group.entries[0],
                count: group.entries.len(),
                collapsed,
            });
            if !collapsed {
                rows.extend(group.entries.into_iter().map(ListRow::Agent));
            }
        }
        rows
    }

    fn selected_index(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.snapshot
            .entries
            .iter()
            .position(|entry| &entry.id == selected)
    }

    fn selected_row(&self) -> Option<usize> {
        let index = self.selected_index()?;
        let key = self.group_by.key(&self.snapshot.entries[index]);
        let matches = matching_indices(&self.snapshot, &self.query);
        if !matches.contains(&index) {
            return None;
        }
        self.list_rows().iter().position(|row| match row {
            ListRow::Agent(other) => *other == index,
            ListRow::Header {
                key: group,
                collapsed: true,
                ..
            } => Some(group) == key.as_ref(),
            _ => false,
        })
    }

    fn selected_group(&self) -> Option<GroupKey> {
        self.selected_row()?;
        let index = self.selected_index()?;
        self.group_by.key(&self.snapshot.entries[index])
    }

    fn fold_selected(&mut self, collapse: bool) {
        if self.current_groups().len() < 2 {
            return;
        }
        if let Some(key) = self.selected_group() {
            if collapse {
                self.collapsed.insert(key);
            } else {
                self.collapsed.remove(&key);
            }
            self.reveal_selection = true;
        }
    }

    fn toggle_group(&mut self, key: GroupKey) {
        if !self.collapsed.remove(&key) {
            self.collapsed.insert(key);
        }
        self.reveal_selection = true;
    }

    fn selection_removed(&mut self) {
        self.selected = None;
        self.selection_missing = true;
        self.message =
            "Selected agent is no longer available. Use ↑/↓ or click to select another.".into();
    }

    fn move_selection(&mut self, down: bool) {
        let rows = self.list_rows();
        let targets: Vec<_> = rows
            .iter()
            .enumerate()
            .filter_map(|(row, value)| match value {
                ListRow::Agent(index)
                | ListRow::Header {
                    first: index,
                    collapsed: true,
                    ..
                } => Some((row, *index)),
                _ => None,
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        let current = self
            .selected_row()
            .and_then(|row| targets.iter().position(|target| target.0 == row));
        let next = match current {
            Some(index) if down => (index + 1).min(targets.len() - 1),
            Some(index) => index.saturating_sub(1),
            None if down => 0,
            None => targets.len() - 1,
        };
        if current == Some(next) {
            // A bounded arrow must not replace a collapsed group's retained member.
            self.reveal_selection = true;
            return;
        }
        self.select_index(targets[next].1);
    }

    fn move_group(&mut self, forward: bool) {
        if self.group_by == GroupBy::None {
            return;
        }
        let groups = self.current_groups();
        if groups.is_empty() {
            return;
        }
        let current = self
            .selected_group()
            .and_then(|key| groups.iter().position(|group| group.key == key));
        let next = match current {
            Some(index) if forward => (index + 1) % groups.len(),
            Some(index) => (index + groups.len() - 1) % groups.len(),
            None if forward => 0,
            None => groups.len() - 1,
        };
        self.select_index(groups[next].entries[0]);
    }

    fn move_attention(&mut self, forward: bool) {
        let indices: Vec<_> = matching_indices(&self.snapshot, &self.query)
            .into_iter()
            .filter(|&index| {
                matches!(
                    self.snapshot.entries[index].status,
                    Status::Blocked | Status::Error
                )
            })
            .collect();
        if indices.is_empty() {
            self.message = "No matching Blocked or Error agents.".into();
            return;
        }
        let current = self.selected_index();
        let index = if forward {
            indices
                .iter()
                .copied()
                .find(|index| current.is_none_or(|current| *index > current))
                .unwrap_or(indices[0])
        } else {
            indices
                .iter()
                .copied()
                .rev()
                .find(|index| current.is_none_or(|current| *index < current))
                .unwrap_or(*indices.last().unwrap())
        };
        if let Some(key) = self.group_by.key(&self.snapshot.entries[index]) {
            self.collapsed.remove(&key);
        }
        self.select_index(index);
    }

    fn select_index(&mut self, index: usize) {
        self.selected = Some(self.snapshot.entries[index].id.clone());
        self.selection_missing = false;
        self.reveal_selection = true;
        self.message.clear();
    }

    fn navigate_selected(&mut self) -> Option<Action> {
        if let Some(row) = self.selected_row() {
            if let ListRow::Header {
                key,
                collapsed: true,
                ..
            } = &self.list_rows()[row]
            {
                self.collapsed.remove(key);
                self.clear_hits();
                self.reveal_selection = true;
                return None;
            }
        } else if self.selected_index().is_some() {
            self.message = "Selected agent is hidden by the filter. Clear the filter or select a visible agent.".into();
            return None;
        }
        if !self.connected {
            self.message = "Rex is disconnected; navigation is disabled.".into();
            return None;
        }
        if self.navigating {
            self.message = "Navigation is already in progress.".into();
            return None;
        }
        let Some(index) = self.selected_index() else {
            if self.selected.is_some() || self.selection_missing {
                self.selection_removed();
            } else {
                self.message = "No agent selected. Use ↑/↓ or click a row.".into();
            }
            return None;
        };
        let entry = self.snapshot.entries[index].clone();
        self.navigating = true;
        self.message = format!("Switching to {}.", entry.location);
        Some(Action::Navigate(Box::new(entry)))
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        self.clear_hits();
        self.viewport = Rect::default();
        let area = frame.area();
        if area.width == 0 || area.height == 0 {
            self.details_limit = 0;
            return;
        }
        self.group_by.sort(&mut self.snapshot);
        if self.selected.is_some() && self.selected_index().is_none() {
            self.selection_removed();
        }
        if self.details {
            self.draw_details(frame);
            return;
        }
        if self.help {
            self.draw_help(frame);
            return;
        }
        let title_height = u16::from(area.height >= 4);
        let header_height = if area.height >= 6 {
            2
        } else {
            u16::from(area.height >= 5)
        };
        let footer_height = area.height.min(2);
        let body = Rect::new(
            area.x,
            area.y + title_height + header_height,
            area.width,
            area.height - title_height - header_height - footer_height,
        );
        self.viewport = body;
        let height = usize::from(body.height);
        let rows = self.list_rows();
        let headers = self.current_groups().len() >= 2;
        let mut hidden = [false; 6];
        if headers {
            match self.group_by {
                GroupBy::None => {}
                GroupBy::Status => hidden[0] = true,
                GroupBy::Agent => hidden[1] = true,
                GroupBy::Cwd => hidden[2] = true,
                GroupBy::Session => hidden[3] = true,
            }
        }
        let selected_row = self.selected_row();
        self.scroll = self.scroll.min(rows.len().saturating_sub(height));
        if height > 0 && self.reveal_selection {
            if let Some(index) = selected_row {
                if index < self.scroll {
                    self.scroll = index;
                } else if index >= self.scroll + height {
                    self.scroll = index + 1 - height;
                }
            }
            self.reveal_selection = false;
        }
        let matching = matching_indices(&self.snapshot, &self.query);
        let home = std::env::var("HOME").ok();
        let measured = measure_columns(
            matching.iter().map(|&index| &self.snapshot.entries[index]),
            home.as_deref(),
        );
        let layout = columns(Rect::new(area.x, body.y, area.width, 1), measured, hidden);
        if title_height > 0 {
            let count = matching.len();
            let total = self.snapshot.entries.len();
            let count = if self.query.is_empty() {
                count.to_string()
            } else {
                format!("{count}/{total}")
            };
            let grouping = if self.group_by == GroupBy::None {
                String::new()
            } else {
                format!("Group: {} | ", self.group_by.label())
            };
            frame.render_widget(
                Paragraph::new(clip_text(
                    &format!("brachio | {grouping}{count} agents"),
                    area.width,
                    false,
                ))
                .style(Style::default().add_modifier(Modifier::BOLD)),
                Rect::new(area.x, area.y, area.width, 1),
            );
        }
        if header_height > 0 {
            let header = Rect::new(area.x, area.y + title_height, area.width, 1);
            for (label, mut column) in COLUMN_LABELS
                .into_iter()
                .zip(layout.areas.into_iter().skip(1))
            {
                column.y = header.y;
                frame.render_widget(
                    Paragraph::new(clip_text(label, column.width, false))
                        .style(Style::default().add_modifier(Modifier::BOLD)),
                    column,
                );
            }
            if header_height == 2 {
                frame.render_widget(
                    Paragraph::new("─".repeat(usize::from(area.width)))
                        .style(Style::default().fg(Color::DarkGray)),
                    Rect::new(area.x, header.y + 1, area.width, 1),
                );
            }
        }
        if rows.is_empty() {
            let text = if self.snapshot.entries.is_empty() {
                "No agents with reported status."
            } else {
                "No agents match the filter. Esc clears it."
            };
            frame.render_widget(Paragraph::new(text), body);
        }
        for (offset, row) in rows.iter().skip(self.scroll).take(height).enumerate() {
            let row_area = Rect::new(body.x, body.y + offset as u16, body.width, 1);
            match row {
                ListRow::Header {
                    key,
                    label,
                    count,
                    collapsed,
                    ..
                } => {
                    let selected = *collapsed && selected_row == Some(self.scroll + offset);
                    let style = if selected {
                        Style::default().bg(Color::DarkGray).fg(Color::White)
                    } else {
                        Style::default().add_modifier(Modifier::BOLD)
                    };
                    let label = if self.group_by == GroupBy::Cwd {
                        home_path(label, home.as_deref())
                    } else {
                        label.clone()
                    };
                    let marker = if *collapsed { "▸" } else { "▾" };
                    let suffix = format!(" ({count})");
                    let text = if row_area.width < suffix.len() as u16 + 2 {
                        count.to_string()
                    } else {
                        let label = clip_text(
                            &label,
                            row_area.width - suffix.len() as u16 - 2,
                            self.group_by == GroupBy::Cwd,
                        );
                        format!("{marker} {label}{suffix}")
                    };
                    frame.render_widget(
                        Paragraph::new(clip_text(&text, row_area.width, false)).style(style),
                        row_area,
                    );
                    self.group_hits.push(GroupHit {
                        area: row_area,
                        key: key.clone(),
                    });
                }
                ListRow::Agent(index) => {
                    let entry = &self.snapshot.entries[*index];
                    let selected = self.selected.as_ref() == Some(&entry.id);
                    let style = if selected {
                        Style::default().bg(Color::DarkGray).fg(Color::White)
                    } else {
                        Style::default()
                    };
                    frame.render_widget(Block::default().style(style), row_area);
                    let [marker, status, agent, cwd, session, tab, pane] =
                        layout.areas.map(|mut column| {
                            column.y = row_area.y;
                            column
                        });
                    frame.render_widget(
                        Paragraph::new(if selected { ">" } else { " " }).style(style),
                        marker,
                    );
                    frame.render_widget(
                        Paragraph::new(clip_text(
                            status_text(entry.status, layout.compact_status),
                            status.width,
                            false,
                        ))
                        .style(style.fg(status_color(entry.status))),
                        status,
                    );
                    frame.render_widget(
                        Paragraph::new(clip_text(&entry.name, agent.width, false)).style(style),
                        agent,
                    );
                    let path = home_path(
                        entry.working_dir.as_deref().unwrap_or("Unknown"),
                        home.as_deref(),
                    );
                    frame.render_widget(
                        Paragraph::new(clip_text(&path, cwd.width, true)).style(style),
                        cwd,
                    );
                    for (label, column) in [
                        (entry.location.session.as_ref(), session),
                        (entry.location.tab.as_ref(), tab),
                        (entry.location.pane.as_ref(), pane),
                    ] {
                        frame.render_widget(
                            Paragraph::new(clip_text(label, column.width, false)).style(style),
                            column,
                        );
                    }
                    self.hits.push(HitRegion {
                        area: row_area,
                        entry: entry.clone(),
                    });
                }
            }
        }
        if footer_height == 2 {
            let text = if self.filter_edit.is_some() {
                clip_text(&format!("/ {}▏", self.query), area.width, true)
            } else {
                shortcuts(area.width, headers)
            };
            frame.render_widget(
                Paragraph::new(text),
                Rect::new(area.x, area.bottom() - 2, area.width, 1),
            );
        }
        let (text, color) = self.footer(area.width);
        frame.render_widget(
            Paragraph::new(text).style(Style::default().fg(color)),
            Rect::new(area.x, area.bottom() - 1, area.width, 1),
        );
    }

    fn footer(&self, width: u16) -> (String, Color) {
        let matching = matching_indices(&self.snapshot, &self.query);
        if self.filter_edit.is_some() {
            let count = format!("{}/{} agents", matching.len(), self.snapshot.entries.len());
            return (
                compact_line(&["Enter Apply", "Esc Cancel", &count], width),
                Color::Cyan,
            );
        }
        let (state, mut color) = if !self.connected {
            ("Disconnected; navigation disabled", Color::Red)
        } else if self.navigating {
            ("Navigating", Color::Yellow)
        } else {
            ("Connected", Color::Green)
        };
        let count = if self.query.is_empty() {
            format!("{} agents", matching.len())
        } else {
            format!("{}/{} agents", matching.len(), self.snapshot.entries.len())
        };
        let warnings = format!("{} warnings (w)", self.snapshot.warnings.len());
        let attention_count = matching
            .iter()
            .filter(|&&index| {
                matches!(
                    self.snapshot.entries[index].status,
                    Status::Blocked | Status::Error
                )
            })
            .count();
        let working_count = matching
            .iter()
            .filter(|&&index| self.snapshot.entries[index].status == Status::Working)
            .count();
        let attention = format!("{attention_count} attention");
        let working = format!("{working_count} working");
        let mut parts = vec![state];
        if !self.snapshot.warnings.is_empty() {
            parts.push(&warnings);
            if self.connected {
                color = Color::Yellow;
            }
        }
        if !self.message.is_empty() && !self.navigating {
            parts.push("Notice (w)");
            if self.connected {
                color = Color::Yellow;
            }
        }
        parts.push(&count);
        if attention_count > 0 {
            parts.push(&attention);
        }
        if working_count > 0 {
            parts.push(&working);
        }
        (compact_line(&parts, width), color)
    }

    fn toggle_details(&mut self) {
        self.details = !self.details;
        self.help = false;
        self.reset_overlay();
    }
    fn close_overlay(&mut self) {
        self.details = false;
        self.help = false;
        self.reset_overlay();
    }
    fn reset_overlay(&mut self) {
        self.details_scroll = 0;
        self.details_limit = 0;
        self.viewport = Rect::default();
        self.clear_hits();
    }
    fn scroll_details(&mut self, down: bool, amount: usize) {
        self.details_scroll = if down {
            self.details_scroll.saturating_add(amount)
        } else {
            self.details_scroll.saturating_sub(amount)
        }
        .min(self.details_limit);
    }

    fn draw_details(&mut self, frame: &mut Frame) {
        let mut values = vec![
            format!(
                "Message: {}",
                if self.message.is_empty() {
                    "No current message."
                } else {
                    &self.message
                }
            ),
            format!(
                "Client ID: {}",
                self.client.as_deref().unwrap_or("Not pinned")
            ),
            format!("Last navigation: {}", self.diagnostic),
            "Session focus is shared with other connected clients.".into(),
            format!("Grouping: {}", self.group_by.label()),
            format!("Filter: {}", self.query),
            format!("Warnings: {}", self.snapshot.warnings.len()),
        ];
        if self.snapshot.warnings.is_empty() {
            values.push("No snapshot warnings.".into());
        }
        for (index, warning) in self.snapshot.warnings.iter().enumerate() {
            values.push(format!("{}. {}", index + 1, warning));
        }
        if let Some(index) = self.selected_index() {
            let entry = &self.snapshot.entries[index];
            values.push(String::new());
            for (label, value) in [
                ("Agent", entry.name.as_str()),
                ("Status", entry.status.label()),
                (
                    "Status source",
                    if entry.records == 0 {
                        "Foreground identity only; no status report"
                    } else {
                        "Reported by Rex program_status"
                    },
                ),
                (
                    "Working directory",
                    entry.working_dir.as_deref().unwrap_or("Unknown"),
                ),
                ("Session", entry.location.session.as_ref()),
                ("Tab", entry.location.tab.as_ref()),
                ("Pane", entry.location.pane.as_ref()),
                ("Session ID", entry.target.session.as_str()),
                ("Tab ID", entry.target.window.as_str()),
                ("Pane ID", entry.target.block.as_str()),
            ] {
                values.push(format!("{label}: {value}"));
            }
        }
        self.draw_overlay(frame, "Details", values);
    }

    fn draw_help(&mut self, frame: &mut Frame) {
        let values = [
            "↑ / ↓ or k / j: Select previous / next visible agent. Collapsed headers are selectable.",
            "Enter: Navigate to the selected visible agent. On a collapsed header, expand only.",
            "g / G (Shift+g): Cycle None, Agent, Status, CWD, Session forward / backward.",
            "Tab / Shift+Tab: Next / previous group, with wraparound. No effect in None.",
            "← / →: Collapse / expand the selected group. Headers appear only for two or more groups.",
            "Mouse: Click a group header to toggle it; click an agent to select and navigate. Wheel scrolls.",
            "/: Edit a case-insensitive filter. All space-separated terms must match.",
            "Filter fields: agent:codex cwd:project session:work status:blocked. Unqualified terms also match Tab and Pane.",
            "Filter editing: Type or paste text, Backspace deletes, Enter applies without navigation, Esc restores the previous filter.",
            "Esc on the list: Clear an active filter, otherwise quit. q quits the list.",
            "a / A: Next / previous matching Blocked or Error agent; expand its group.",
            "w: Full selected-agent values, status source, exact IDs, client diagnostics, messages, and warnings.",
            "?: Keyboard help. q / Esc closes details or help. Ctrl-C always quits.",
            "Details / help: ↑ / ↓, PageUp / PageDown, or wheel scrolls.",
            "Statuses: ! Blocked, × Error, ✓ Done, ● Working, ○ Idle.",
            "Unrecognized states use Idle, as the Rex schema requires.",
            "Foreground identity without reports uses an Idle fallback. Details identifies the no-report source.",
            "Blocked is not proof that input is needed. Done is a reported state, not whole-session completion.",
        ].into_iter().map(String::from).collect();
        self.draw_overlay(frame, "Help", values);
    }

    fn draw_overlay(&mut self, frame: &mut Frame, title: &str, values: Vec<String>) {
        let area = frame.area();
        let title_height = u16::from(area.height >= 3);
        let body = Rect::new(
            area.x,
            area.y + title_height,
            area.width,
            area.height - title_height - 1,
        );
        self.viewport = body;
        let lines: Vec<Line<'static>> = values
            .iter()
            .flat_map(|value| wrap_details(&display_text(value), body.width))
            .collect();
        let count = lines.len();
        let height = usize::from(body.height);
        self.details_limit = count.saturating_sub(height);
        self.details_scroll = self.details_scroll.min(self.details_limit);
        if title_height > 0 {
            frame.render_widget(
                Paragraph::new(clip_text(
                    &format!(
                        "brachio | {title} | lines {}/{count}",
                        self.details_scroll + 1
                    ),
                    area.width,
                    false,
                ))
                .style(Style::default().add_modifier(Modifier::BOLD)),
                Rect::new(area.x, area.y, area.width, 1),
            );
        }
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(self.details_scroll)
                    .take(height)
                    .collect::<Vec<_>>(),
            ),
            body,
        );
        frame.render_widget(
            Paragraph::new(clip_text(
                "↑↓/Wheel Scroll · Esc/q Back · Ctrl-C Quit",
                area.width,
                false,
            )),
            Rect::new(area.x, area.bottom() - 1, area.width, 1),
        );
    }
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x && x < area.right() && y >= area.y && y < area.bottom()
}

#[cfg(test)]
mod tests;
