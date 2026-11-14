use std::collections::BTreeSet;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};

use crate::model::{Entry, EntryId, Snapshot, Status, display_text};

mod list;
mod presentation;
use list::{Group, GroupBy, GroupKey, groups, matching_indices};
use presentation::{
    STATUSES, clip_text, column_label, columns, compact_line, compact_spans, field_lines,
    home_path, measure_columns, plural, shortcuts, spans_width, status_fg, status_summary,
    status_text,
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

/// A short-lived response to the latest action. The next list input clears it.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Feedback {
    text: String,
    warning: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Motion {
    Previous,
    Next,
    PageUp,
    PageDown,
    First,
    Last,
}

const SELECTED_BG: Color = Color::DarkGray;
const SELECTION_REMOVED: &str =
    "Selected agent is no longer available. Use ↑/↓ or click to select another.";

/// View and input state only. The caller supplies snapshots and runs navigation.
#[derive(Default)]
pub struct App {
    pub snapshot: Snapshot,
    /// A persistent condition reported by the caller, such as a connection or
    /// client error. Empty while healthy. Feedback for user actions is separate.
    pub message: String,
    pub connected: bool,
    pub navigating: bool,
    pub client: Option<String>,
    pub diagnostic: String,
    feedback: Option<Feedback>,
    jump_target: Option<String>,
    group_by: GroupBy,
    collapsed: BTreeSet<GroupKey>,
    query: String,
    filter_edit: Option<String>,
    filter_edit_selection: Option<EntryId>,
    filter_edit_selection_missing: bool,
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
    page: usize,
    /// The drawn scrollbar column. Clicks there select nothing.
    scrollbar: Option<u16>,
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

    /// Record a failed refresh. Before the first snapshot, Brachio is still connecting.
    pub fn refresh_failed(&mut self, error: &str) {
        self.connected = false;
        self.message = if self.received_snapshot {
            format!("Rex disconnected; showing the last known agents; navigation disabled: {error}")
        } else {
            format!("Cannot reach Rex; retrying: {error}")
        };
    }

    /// Record the outcome of a navigation request, or a request that could not be sent.
    pub fn navigation_finished(&mut self, result: Result<(), String>) {
        self.navigating = false;
        let target = self.jump_target.take();
        match result {
            Ok(()) => {
                self.diagnostic =
                    "Rex accepted app selection; native display is not verified by Brachio".into();
                if let Some(target) = target {
                    self.set_feedback(format!("Rex accepted the jump to {target}."), false);
                }
            }
            Err(error) => {
                self.diagnostic = error.clone();
                self.set_feedback(error, true);
            }
        }
    }

    /// The text the footer shows: action feedback, then the persistent message,
    /// then a reminder that the selected agent disappeared.
    pub fn notice(&self) -> &str {
        if let Some(feedback) = &self.feedback {
            &feedback.text
        } else if !self.message.is_empty() || !self.selection_missing {
            &self.message
        } else {
            SELECTION_REMOVED
        }
    }

    fn set_feedback(&mut self, text: impl Into<String>, warning: bool) {
        self.feedback = Some(Feedback {
            text: text.into(),
            warning,
        });
    }

    fn clear_hits(&mut self) {
        self.hits.clear();
        self.group_hits.clear();
    }

    fn agent_at(&self, x: u16, y: u16) -> Option<&Entry> {
        if self.scrollbar == Some(x) {
            return None;
        }
        self.hits
            .iter()
            .find(|hit| contains(hit.area, x, y))
            .map(|hit| &hit.entry)
    }

    fn group_at(&self, x: u16, y: u16) -> Option<&GroupKey> {
        if self.scrollbar == Some(x) {
            return None;
        }
        self.group_hits
            .iter()
            .find(|hit| contains(hit.area, x, y))
            .map(|hit| &hit.key)
    }

    pub fn discard_redraw_click(&mut self) {
        if !self.details && !self.help {
            self.set_feedback(
                "List redrawn; click again to select the displayed row",
                true,
            );
        }
    }

    pub fn handle_event(&mut self, event: Event) -> Option<Action> {
        // List input replaces the previous feedback. Opening an overlay keeps it so
        // Details can show it, and overlays return to the same footer.
        let overlay = self.details || self.help;
        let clears = !overlay
            && match &event {
                Event::Key(key) => {
                    key.kind != KeyEventKind::Release
                        && (self.filter_edit.is_some()
                            || !matches!(key.code, KeyCode::Char('w' | '?')))
                }
                // A click on empty space or a stale map does nothing, so it keeps feedback.
                Event::Mouse(mouse) => {
                    matches!(mouse.kind, MouseEventKind::Down(_))
                        && (self.agent_at(mouse.column, mouse.row).is_some()
                            || self.group_at(mouse.column, mouse.row).is_some())
                }
                Event::Paste(_) => self.filter_edit.is_some(),
                _ => false,
            };
        if clears {
            self.feedback = None;
        }
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
                        self.selected = self.filter_edit_selection.take();
                        self.selection_missing = self.filter_edit_selection_missing;
                        if self.selected.is_some() && self.selected_index().is_none() {
                            self.selection_removed();
                        }
                        self.clear_hits();
                        self.scroll = 0;
                        self.reveal_selection = true;
                    }
                    KeyCode::Enter => {
                        self.filter_edit = None;
                        self.filter_edit_selection = None;
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
                        self.move_selection(if key.code == KeyCode::Down {
                            Motion::Next
                        } else {
                            Motion::Previous
                        });
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
                    KeyCode::Home | KeyCode::End => {
                        self.scroll_details(key.code == KeyCode::End, usize::MAX)
                    }
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
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::Enter
                    | KeyCode::Char('j' | 'k' | 'g' | 'G' | 'a' | 'A' | '/' | '?' | 'w')
            ) || (self.group_by != GroupBy::None
                && matches!(key.code, KeyCode::Tab | KeyCode::BackTab));
            if mutating {
                self.clear_hits();
            }
            let motion = match key.code {
                KeyCode::Up | KeyCode::Char('k') => Some(Motion::Previous),
                KeyCode::Down | KeyCode::Char('j') => Some(Motion::Next),
                KeyCode::PageUp => Some(Motion::PageUp),
                KeyCode::PageDown => Some(Motion::PageDown),
                KeyCode::Home => Some(Motion::First),
                KeyCode::End => Some(Motion::Last),
                _ => None,
            };
            if let Some(motion) = motion {
                self.move_selection(motion);
                return None;
            }
            return match key.code {
                KeyCode::Char('q') => Some(Action::Quit),
                KeyCode::Esc if !self.query.is_empty() => {
                    self.query.clear();
                    self.filter_changed();
                    None
                }
                KeyCode::Esc => {
                    // Escape backs out of views; it never quits a long-running monitor.
                    self.set_feedback("Press q to quit.", false);
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
                    self.filter_edit_selection = self.selected.clone();
                    self.filter_edit_selection_missing = self.selection_missing;
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
                    let group = self.group_at(mouse.column, mouse.row).cloned();
                    let clicked = self.agent_at(mouse.column, mouse.row).cloned();
                    self.clear_hits();
                    if let Some(key) = group {
                        if self.current_groups().iter().any(|group| group.key == key) {
                            self.toggle_group(key);
                        }
                        return None;
                    }
                    let entry = clicked?;
                    if !self.snapshot.entries.contains(&entry) {
                        self.set_feedback(
                            "The list changed. Draw the list again before clicking.",
                            true,
                        );
                        return None;
                    }
                    self.selected = Some(entry.id);
                    self.selection_missing = false;
                    self.filter_edit = None;
                    self.filter_edit_selection = None;
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

    /// A deliberate filter change selects the first match when the current
    /// selection is hidden, so Enter after filtering jumps to a visible agent.
    fn filter_changed(&mut self) {
        self.clear_hits();
        self.scroll = 0;
        self.reveal_selection = true;
        if self.selected_row().is_none()
            && let Some(&(_, index)) = self.selection_targets().first()
        {
            self.select_index(index);
        }
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

    /// Selectable rows as `(row, entry index)`: agents and collapsed headers.
    fn selection_targets(&self) -> Vec<(usize, usize)> {
        self.list_rows()
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
            .collect()
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
        if !self.selection_missing {
            self.set_feedback(SELECTION_REMOVED, true);
        }
        self.selection_missing = true;
    }

    fn move_selection(&mut self, motion: Motion) {
        let targets = self.selection_targets();
        if targets.is_empty() {
            return;
        }
        let last = targets.len() - 1;
        let current = self
            .selected_row()
            .and_then(|row| targets.iter().position(|target| target.0 == row));
        let page = self.page.max(1);
        let next = match (motion, current) {
            (Motion::First, _) => 0,
            (Motion::Last, _) => last,
            (Motion::Next, Some(index)) => (index + 1).min(last),
            (Motion::Previous, Some(index)) => index.saturating_sub(1),
            (Motion::PageDown, Some(index)) => {
                let row = targets[index].0 + page;
                targets
                    .iter()
                    .position(|target| target.0 >= row)
                    .unwrap_or(last)
            }
            (Motion::PageUp, Some(index)) => {
                let row = targets[index].0.saturating_sub(page);
                targets
                    .iter()
                    .rposition(|target| target.0 <= row)
                    .unwrap_or(0)
            }
            (Motion::Next | Motion::PageDown, None) => 0,
            (Motion::Previous | Motion::PageUp, None) => last,
        };
        if current == Some(next) {
            // A bounded move must not replace a collapsed group's retained member.
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
            self.set_feedback("No matching Blocked or Error agents.", false);
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
        self.feedback = None;
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
            self.set_feedback(
                "Selected agent is hidden by the filter. Clear the filter or select a visible agent.",
                true,
            );
            return None;
        }
        if !self.connected {
            self.set_feedback("Rex is disconnected; navigation is disabled.", true);
            return None;
        }
        if self.navigating {
            self.set_feedback("Navigation is already in progress.", false);
            return None;
        }
        let Some(index) = self.selected_index() else {
            if self.selected.is_some() || self.selection_missing {
                self.selection_removed();
            } else {
                self.set_feedback("No agent selected. Use ↑/↓ or click a row.", false);
            }
            return None;
        };
        let entry = self.snapshot.entries[index].clone();
        self.navigating = true;
        let target = entry.location.to_string();
        self.set_feedback(format!("Jumping to {target}…"), false);
        self.jump_target = Some(target);
        Some(Action::Navigate(Box::new(entry)))
    }

    /// Listed values are stale after a failed refresh that followed a snapshot.
    fn stale(&self) -> bool {
        !self.connected && self.received_snapshot
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        self.clear_hits();
        self.scrollbar = None;
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
        self.page = height.max(1);
        let rows = self.list_rows();
        let groups = self.current_groups();
        let headers = groups.len() >= 2;
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
        // The rightmost cell is reserved for the scrollbar whether or not the
        // list overflows, so folding or filtering never shifts the columns.
        let layout = columns(column_area(body), measured, hidden);
        if title_height > 0 {
            frame.render_widget(
                Paragraph::new(self.title(area.width, &matching)),
                Rect::new(area.x, area.y, area.width, 1),
            );
        }
        if header_height > 0 {
            let header = Rect::new(area.x, area.y + title_height, area.width, 1);
            for (index, mut column) in layout.areas.into_iter().skip(1).enumerate() {
                column.y = header.y;
                frame.render_widget(
                    Paragraph::new(clip_text(
                        column_label(index, column.width),
                        column.width,
                        false,
                    ))
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
            let text = if !self.received_snapshot {
                "Connecting to Rex…"
            } else if self.snapshot.entries.is_empty() {
                "No agents with reported status."
            } else {
                "No agents match the filter. Esc clears it."
            };
            frame.render_widget(Paragraph::new(text), body);
        }
        let dim = if self.stale() {
            Modifier::DIM
        } else {
            Modifier::empty()
        };
        for (offset, row) in rows.iter().skip(self.scroll).take(height).enumerate() {
            let row_area = Rect::new(body.x, body.y + offset as u16, body.width, 1);
            match row {
                ListRow::Header { key, collapsed, .. } => {
                    let selected = *collapsed && selected_row == Some(self.scroll + offset);
                    let members = groups
                        .iter()
                        .find(|group| &group.key == key)
                        .map_or(&[][..], |group| group.entries.as_slice());
                    let line = self.header_line(
                        row,
                        members,
                        selected,
                        column_area(row_area).width,
                        home.as_deref(),
                    );
                    let style = if selected {
                        Style::default().bg(SELECTED_BG).fg(Color::White)
                    } else {
                        Style::default()
                    };
                    frame.render_widget(
                        Paragraph::new(line).style(style.add_modifier(dim)),
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
                        Style::default().bg(SELECTED_BG).fg(Color::White)
                    } else {
                        Style::default()
                    }
                    .add_modifier(dim);
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
                        .style(style.fg(status_fg(entry.status, selected))),
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
        if rows.len() > height && height > 0 && body.width >= 3 {
            draw_scrollbar(frame, body, self.scroll, rows.len());
            self.scrollbar = Some(body.right() - 1);
        }
        if footer_height == 2 {
            let line = if self.filter_edit.is_some() {
                Line::raw(clip_text(&format!("/ {}▏", self.query), area.width, true))
            } else {
                let attention = matching
                    .iter()
                    .filter(|&&index| {
                        matches!(
                            self.snapshot.entries[index].status,
                            Status::Blocked | Status::Error
                        )
                    })
                    .count();
                shortcuts(area.width, headers, attention)
            };
            frame.render_widget(
                Paragraph::new(line),
                Rect::new(area.x, area.bottom() - 2, area.width, 1),
            );
        }
        frame.render_widget(
            Paragraph::new(self.footer(area.width)),
            Rect::new(area.x, area.bottom() - 1, area.width, 1),
        );
    }

    /// Group header: fold marker, label, count, and a status summary when it fits.
    fn header_line(
        &self,
        row: &ListRow,
        members: &[usize],
        selected: bool,
        width: u16,
        home: Option<&str>,
    ) -> Line<'static> {
        let ListRow::Header {
            label,
            count,
            collapsed,
            ..
        } = row
        else {
            return Line::default();
        };
        let (count, collapsed) = (*count, *collapsed);
        // Headers start one marker column left of agent rows to keep the hierarchy.
        // The full-row highlight marks a selected collapsed header.
        let available = usize::from(width);
        let suffix = format!(" ({count})");
        if available < suffix.len() + 2 {
            return Line::raw(clip_text(&count.to_string(), width, false));
        }
        let label = if self.group_by == GroupBy::Cwd {
            home_path(label, home)
        } else {
            label.to_owned()
        };
        // A summary only adds information when members are hidden or differ;
        // a single-status group's count already says everything.
        let statuses: Vec<_> = members
            .iter()
            .map(|&index| self.snapshot.entries[index].status)
            .collect();
        let mixed = statuses.windows(2).any(|pair| pair[0] != pair[1]);
        let summary = if self.group_by == GroupBy::Status || !(collapsed || mixed) {
            Vec::new()
        } else {
            status_summary(statuses, selected)
        };
        let summary_width = spans_width(&summary);
        let base = 2 + suffix.len();
        let label_width = Line::raw(label.as_str()).width();
        let show_summary =
            summary_width > 0 && available >= base + label_width.min(8) + 2 + summary_width;
        let room = available - base - if show_summary { 2 + summary_width } else { 0 };
        let label = clip_text(&label, room as u16, self.group_by == GroupBy::Cwd);
        let mut spans = vec![
            Span::raw(if collapsed { "▸ " } else { "▾ " }),
            Span::styled(label, Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(suffix),
        ];
        if show_summary {
            spans.push(Span::raw("  "));
            spans.extend(summary);
        }
        Line::from(spans)
    }

    /// Title: name, agent count, colored status counts, grouping, and active
    /// filter. Status counts drop first when narrow, then grouping; the filter clips.
    fn title(&self, width: u16, matching: &[usize]) -> Line<'static> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let count = self.received_snapshot.then(|| {
            let total = self.snapshot.entries.len();
            if self.query.is_empty() {
                plural(total, "agent", "agents")
            } else {
                format!("{}/{}", matching.len(), plural(total, "agent", "agents"))
            }
        });
        let filter = (!self.query.is_empty() && self.filter_edit.is_none())
            .then(|| format!("Filter: {}", display_text(&self.query)));
        let group =
            (self.group_by != GroupBy::None).then(|| format!("Group: {}", self.group_by.label()));
        let summary = status_summary(
            matching
                .iter()
                .map(|&index| self.snapshot.entries[index].status),
            false,
        );
        let width = usize::from(width);
        let joined = |parts: &[String]| {
            parts.iter().fold(String::new(), |mut text, part| {
                text.push_str(" · ");
                text.push_str(part);
                text
            })
        };
        let cells = |text: &str| Line::raw(text).width();
        let head = format!("brachio{}", joined(&Vec::from_iter(count)));
        let mut rest: Vec<String> = group.iter().chain(&filter).cloned().collect();
        let summary_width = spans_width(&summary);
        let show_summary =
            summary_width > 0 && cells(&head) + 3 + summary_width + cells(&joined(&rest)) <= width;
        if show_summary {
            // Everything fits, so nothing below needs clipping.
            let mut spans = vec![
                Span::styled("brachio", bold),
                Span::raw(head["brachio".len()..].to_owned()),
                Span::raw(" · "),
            ];
            spans.extend(summary);
            spans.push(Span::raw(joined(&rest)));
            return Line::from(spans);
        }
        if cells(&head) + cells(&joined(&rest)) > width {
            rest = Vec::from_iter(filter.clone());
        }
        if let Some(filter) = &filter
            && cells(&head) + cells(&joined(&rest)) > width
        {
            let room = width.saturating_sub(cells(&head) + 3);
            rest = if room >= 8 {
                vec![clip_text(filter, room as u16, false)]
            } else {
                Vec::new()
            };
        }
        let text = clip_text(&(head + &joined(&rest)), width as u16, false);
        match text.strip_prefix("brachio") {
            Some(rest) => Line::from(vec![
                Span::styled("brachio", bold),
                Span::raw(rest.to_owned()),
            ]),
            None => Line::styled(text, bold),
        }
    }

    fn connection_state(&self) -> (&'static str, Color) {
        if !self.connected && !self.received_snapshot {
            ("Connecting", Color::Yellow)
        } else if !self.connected {
            ("Disconnected", Color::Red)
        } else if self.navigating {
            ("Navigating", Color::Yellow)
        } else {
            ("Connected", Color::Green)
        }
    }

    /// Status line: connection state, warning count, and the current notice.
    /// A notice that does not fit is clipped and points to Details.
    fn footer(&self, width: u16) -> Line<'static> {
        let matching = matching_indices(&self.snapshot, &self.query);
        if self.filter_edit.is_some() {
            let count = format!("{}/{} agents", matching.len(), self.snapshot.entries.len());
            return Line::styled(
                compact_line(&["Enter Apply", "Esc Cancel", "↑↓ Select", &count], width),
                Style::default().fg(Color::Cyan),
            );
        }
        let (state, color) = self.connection_state();
        let mut parts = vec![Span::styled(state, Style::default().fg(color))];
        let warnings = self.snapshot.warnings.len();
        if warnings > 0 {
            parts.push(Span::styled(
                format!("{} (w)", plural(warnings, "warning", "warnings")),
                Style::default().fg(Color::Yellow),
            ));
        }
        let mut line = compact_spans(&parts, width);
        let notice = display_text(self.notice());
        let used = line.width();
        let room = usize::from(width).saturating_sub(used + 3);
        if !notice.is_empty() && room > 0 && used > 0 {
            let style = match &self.feedback {
                Some(Feedback { warning: true, .. }) => Style::default().fg(Color::Yellow),
                Some(_) => Style::default(),
                None if !self.connected => Style::default().fg(Color::Red),
                None => Style::default().fg(Color::Yellow),
            };
            let size = Line::raw(notice.as_str()).width();
            let text = if size <= room {
                Some(notice)
            } else if room >= 8 {
                Some(format!(
                    "{} (w)",
                    clip_text(&notice, room as u16 - 4, false)
                ))
            } else {
                None
            };
            if let Some(text) = text {
                line.push_span(Span::raw(" · "));
                line.push_span(Span::styled(text, style));
            }
        }
        line
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
        let width = frame.area().width;
        let heading = Style::default().add_modifier(Modifier::BOLD);
        let label = Style::default().fg(Color::Gray);
        let none = |value: &str| {
            if value.is_empty() {
                "None".to_owned()
            } else {
                value.to_owned()
            }
        };
        let mut lines = vec![Line::styled("Selected agent", heading)];
        let field = |lines: &mut Vec<Line<'static>>, name: &str, value: &str| {
            lines.extend(field_lines(name, value, 17, width, label, Style::default()));
        };
        if let Some(index) = self.selected_index() {
            let entry = &self.snapshot.entries[index];
            for (name, value) in [
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
                field(&mut lines, name, value);
            }
        } else {
            lines.extend(field_lines(
                "",
                "No agent selected.",
                0,
                width,
                label,
                Style::default(),
            ));
        }
        lines.push(Line::default());
        lines.push(Line::styled("Activity", heading));
        let latest = self
            .feedback
            .as_ref()
            .map_or_else(String::new, |feedback| feedback.text.clone());
        field(&mut lines, "Latest", &none(&latest));
        field(&mut lines, "Connection", &none(&self.message));
        field(&mut lines, "Last navigation", &none(&self.diagnostic));
        lines.push(Line::default());
        lines.push(Line::styled("Rex", heading));
        field(&mut lines, "State", self.connection_state().0);
        field(
            &mut lines,
            "Client ID",
            self.client.as_deref().unwrap_or("Not pinned"),
        );
        field(
            &mut lines,
            "Shared focus",
            "Session focus is shared with other connected clients.",
        );
        lines.push(Line::default());
        lines.push(Line::styled("View", heading));
        field(&mut lines, "Grouping", self.group_by.label());
        field(&mut lines, "Filter", &none(&self.query));
        lines.push(Line::default());
        lines.push(Line::styled(
            format!("Warnings ({})", self.snapshot.warnings.len()),
            heading,
        ));
        if self.snapshot.warnings.is_empty() {
            lines.extend(field_lines(
                "",
                "No snapshot warnings.",
                0,
                width,
                label,
                Style::default(),
            ));
        }
        let number_width = format!("{}.", self.snapshot.warnings.len()).len();
        for (index, warning) in self.snapshot.warnings.iter().enumerate() {
            let number = format!("{}.", index + 1);
            lines.extend(field_lines(
                &number,
                warning,
                number_width,
                width,
                label,
                Style::default(),
            ));
        }
        self.draw_overlay(frame, "Details", lines);
    }

    fn draw_help(&mut self, frame: &mut Frame) {
        const SECTIONS: &[(&str, &[(&str, &str)])] = &[
            (
                "Move",
                &[
                    (
                        "↑ ↓  k j",
                        "Select the previous or next row. Collapsed group headers are selectable.",
                    ),
                    ("PgUp PgDn", "Move the selection one page."),
                    ("Home End", "Select the first or last row."),
                    (
                        "a  A",
                        "Select the next or previous matching Blocked or Error agent and expand its group.",
                    ),
                    ("Wheel", "Scroll the list without changing the selection."),
                ],
            ),
            (
                "Jump",
                &[
                    (
                        "Enter",
                        "Jump to the selected agent in Rex. On a collapsed group, expand only.",
                    ),
                    (
                        "Click",
                        "Jump to an agent. Clicking a group header only collapses or expands it.",
                    ),
                ],
            ),
            (
                "Group",
                &[
                    (
                        "g  G",
                        "Cycle grouping forward or backward: None, Agent, Status, CWD, Session.",
                    ),
                    (
                        "Tab  Shift+Tab",
                        "Select the next or previous group, with wraparound.",
                    ),
                    ("←  →", "Collapse or expand the selected group."),
                ],
            ),
            (
                "Filter",
                &[
                    (
                        "/",
                        "Edit the filter. The first match is selected when the selection is hidden.",
                    ),
                    (
                        "Enter  Esc",
                        "While editing: apply without jumping, or restore the previous filter.",
                    ),
                    ("Esc", "Clear the active filter."),
                    (
                        "Terms",
                        "Case-insensitive; every space-separated term must match. Prefixes: agent: cwd: session: status:. Other terms also match Tab and Pane.",
                    ),
                ],
            ),
            (
                "Views",
                &[
                    (
                        "w",
                        "Details: selected agent, exact IDs, messages, and warnings.",
                    ),
                    ("?", "This help."),
                    ("Esc  q", "Close Details or Help."),
                    ("q", "Quit from the list. Ctrl-C quits from any view."),
                ],
            ),
        ];
        let width = frame.area().width;
        let heading = Style::default().add_modifier(Modifier::BOLD);
        let key = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let mut lines = Vec::new();
        for (title, rows) in SECTIONS {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            lines.push(Line::styled(*title, heading));
            for (keys, description) in *rows {
                lines.extend(field_lines(
                    keys,
                    description,
                    14,
                    width,
                    key,
                    Style::default(),
                ));
            }
        }
        lines.push(Line::default());
        lines.push(Line::styled("Statuses", heading));
        let mut legend = vec![Span::raw(if width >= 8 { "  " } else { "" })];
        for status in STATUSES {
            if legend.len() > 1 {
                legend.push(Span::raw("  "));
            }
            legend.push(Span::styled(
                status_text(status, false),
                Style::default().fg(status_fg(status, false)),
            ));
        }
        if Line::from(legend.clone()).width() <= usize::from(width) {
            lines.push(Line::from(legend));
        } else {
            for status in STATUSES {
                lines.push(Line::styled(
                    status_text(status, false),
                    Style::default().fg(status_fg(status, false)),
                ));
            }
        }
        lines.extend(field_lines(
            "",
            "Unrecognized states and agents without a status report use Idle, as the Rex schema requires. Details shows the status source.",
            0,
            width,
            key,
            Style::default(),
        ));
        self.draw_overlay(frame, "Help", lines);
    }

    fn draw_overlay(&mut self, frame: &mut Frame, title: &str, lines: Vec<Line<'static>>) {
        let area = frame.area();
        let title_height = u16::from(area.height >= 3);
        let body = Rect::new(
            area.x,
            area.y + title_height,
            area.width,
            area.height - title_height - 1,
        );
        self.viewport = body;
        let count = lines.len();
        let height = usize::from(body.height);
        self.details_limit = count.saturating_sub(height);
        self.details_scroll = self.details_scroll.min(self.details_limit);
        let scrollable = self.details_limit > 0;
        if title_height > 0 {
            let text = clip_text(&format!("brachio · {title}"), area.width, false);
            let mut spans = vec![Span::styled(
                text.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )];
            if scrollable {
                let position = format!(
                    "lines {}–{} of {count}",
                    self.details_scroll + 1,
                    (self.details_scroll + height).min(count)
                );
                let used = Line::raw(text.as_str()).width();
                let size = Line::raw(position.as_str()).width();
                if used + 2 + size <= usize::from(area.width) {
                    spans.push(Span::raw(" ".repeat(usize::from(area.width) - used - size)));
                    spans.push(Span::styled(position, Style::default().fg(Color::Gray)));
                }
            }
            frame.render_widget(
                Paragraph::new(Line::from(spans)),
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
        let hints: &[&str] = if scrollable {
            &["↑↓/PgUp/PgDn Scroll", "Esc/q Back", "Ctrl-C Quit"]
        } else {
            &["Esc/q Back", "Ctrl-C Quit"]
        };
        frame.render_widget(
            Paragraph::new(compact_line(hints, area.width)),
            Rect::new(area.x, area.bottom() - 1, area.width, 1),
        );
    }
}

/// Columns use the list width minus one reserved scrollbar cell.
fn column_area(body: Rect) -> Rect {
    let reserve = u16::from(body.width >= 3);
    Rect::new(body.x, body.y, body.width - reserve, 1)
}

/// A proportional thumb in the reserved rightmost column.
fn draw_scrollbar(frame: &mut Frame, body: Rect, scroll: usize, rows: usize) {
    let height = usize::from(body.height);
    let max_scroll = rows - height;
    let thumb = ((height * height + rows / 2) / rows).clamp(1, height);
    let start = (scroll * (height - thumb) * 2 + max_scroll) / (2 * max_scroll);
    let x = body.right() - 1;
    let buffer = frame.buffer_mut();
    for offset in 0..height {
        let cell = &mut buffer[(x, body.y + offset as u16)];
        if (start..start + thumb).contains(&offset) {
            cell.set_symbol("┃").set_fg(Color::Gray);
        } else {
            cell.set_symbol("│").set_fg(Color::DarkGray);
        }
    }
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x && x < area.right() && y >= area.y && y < area.bottom()
}

#[cfg(test)]
mod tests;
