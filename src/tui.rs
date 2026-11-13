use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};

use crate::model::{Entry, EntryId, Snapshot, Status, plain};

#[derive(Debug)]
pub enum Action {
    Quit,
    Navigate(Box<Entry>),
}

struct HitRegion {
    area: Rect,
    entry: Entry,
}

/// View and input state only. The caller supplies snapshots and runs navigation.
#[derive(Default)]
pub struct App {
    pub snapshot: Snapshot,
    pub message: String,
    pub connected: bool,
    pub navigating: bool,
    selected: Option<EntryId>,
    selection_missing: bool,
    received_snapshot: bool,
    scroll: usize,
    reveal_selection: bool,
    details: bool,
    details_scroll: usize,
    details_limit: usize,
    viewport: Rect,
    hits: Vec<HitRegion>,
}

impl App {
    /// Use this method for updates so that clicks cannot use an old list layout.
    pub fn replace_snapshot(&mut self, mut snapshot: Snapshot) {
        snapshot.sort();
        self.hits.clear();
        let old_index = self.selected_index();
        self.snapshot = snapshot;
        if self.selection_missing || (self.selected.is_some() && self.selected_index().is_none()) {
            self.selection_removed();
        } else if !self.received_snapshot {
            self.selected = self.snapshot.entries.first().map(|entry| entry.id.clone());
        }
        self.reveal_selection |= !self.received_snapshot || old_index != self.selected_index();
        self.received_snapshot = true;
    }

    pub fn handle_event(&mut self, event: Event) -> Option<Action> {
        // Resolve clicks from the last draw, never from a newly sorted row index.
        let clicked = match &event {
            Event::Mouse(mouse)
                if !self.details && mouse.kind == MouseEventKind::Down(MouseButton::Left) =>
            {
                self.hits
                    .iter()
                    .find(|hit| contains(hit.area, mouse.column, mouse.row))
                    .map(|hit| hit.entry.clone())
            }
            _ => None,
        };
        // Preserve targets for motion, release, and other non-mutating input.
        // Selection, scroll, and resize need a draw before another click.
        let invalidates = match &event {
            Event::Key(key) => {
                key.kind != KeyEventKind::Release
                    && matches!(key.code, KeyCode::Up | KeyCode::Down | KeyCode::Enter)
            }
            Event::Mouse(mouse) => matches!(
                mouse.kind,
                MouseEventKind::Down(MouseButton::Left)
                    | MouseEventKind::ScrollDown
                    | MouseEventKind::ScrollUp
            ),
            Event::Resize(_, _) => true,
            _ => false,
        };
        if invalidates {
            self.hits.clear();
        }

        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Char('c' | 'C') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    Some(Action::Quit)
                }
                KeyCode::Esc if self.details => {
                    self.toggle_details();
                    None
                }
                KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
                KeyCode::Char('w') => {
                    self.toggle_details();
                    None
                }
                KeyCode::Up | KeyCode::Down if self.details => {
                    self.scroll_details(key.code == KeyCode::Down, 1);
                    None
                }
                KeyCode::Up => {
                    self.move_selection(false);
                    None
                }
                KeyCode::Down => {
                    self.move_selection(true);
                    None
                }
                KeyCode::Enter if !self.details => self.navigate_selected(),
                _ => None,
            },
            Event::Mouse(mouse) if self.details => {
                if matches!(
                    mouse.kind,
                    MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                ) && contains(self.viewport, mouse.column, mouse.row)
                {
                    self.scroll_details(mouse.kind == MouseEventKind::ScrollDown, 3);
                }
                None
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let entry = clicked?;
                    if !self.snapshot.entries.contains(&entry) {
                        self.message =
                            "The list changed. Draw the list again before clicking.".into();
                        return None;
                    }
                    self.selected = Some(entry.id);
                    self.selection_missing = false;
                    self.message.clear();
                    self.navigate_selected()
                }
                MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                    if contains(self.viewport, mouse.column, mouse.row) =>
                {
                    let limit = self
                        .snapshot
                        .entries
                        .len()
                        .saturating_sub(usize::from(self.viewport.height));
                    self.scroll = if mouse.kind == MouseEventKind::ScrollDown {
                        self.scroll.saturating_add(3).min(limit)
                    } else {
                        self.scroll.saturating_sub(3).min(limit)
                    };
                    // Scrolling does not choose a navigation target.
                    self.reveal_selection = false;
                    None
                }
                _ => None,
            },
            Event::Resize(_, _) => {
                self.viewport = Rect::default();
                if !self.details {
                    self.reveal_selection = true;
                }
                None
            }
            _ => None,
        }
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        self.hits.clear();
        self.viewport = Rect::default();
        let area = frame.area();
        if area.width == 0 || area.height == 0 {
            self.details_limit = 0;
            return;
        }
        self.snapshot.sort();
        if self.selected.is_some() && self.selected_index().is_none() {
            self.selection_removed();
        }
        if self.details {
            self.draw_details(frame);
            return;
        }

        // Keep the status and help visible. Omit headings in very short views.
        let title_height = u16::from(area.height >= 4);
        let header_height = u16::from(area.height >= 5);
        let footer_height = area.height.min(2);
        let body = Rect::new(
            area.x,
            area.y + title_height + header_height,
            area.width,
            area.height - title_height - header_height - footer_height,
        );
        self.viewport = body;
        let height = usize::from(body.height);
        self.scroll = self
            .scroll
            .min(self.snapshot.entries.len().saturating_sub(height));
        if height > 0 && self.reveal_selection {
            if let Some(index) = self.selected_index() {
                if index < self.scroll {
                    self.scroll = index;
                } else if index >= self.scroll + height {
                    self.scroll = index + 1 - height;
                }
            }
            self.reveal_selection = false;
        }

        if title_height > 0 {
            let count = self.snapshot.entries.len();
            let first = if count == 0 { 0 } else { self.scroll + 1 };
            let last = (self.scroll + height).min(count);
            frame.render_widget(
                Paragraph::new(format!("brachio | {count} agents | {first}-{last}"))
                    .style(Style::default().add_modifier(Modifier::BOLD)),
                Rect::new(area.x, area.y, area.width, 1),
            );
        }
        if header_height > 0 {
            let header = Rect::new(area.x, area.y + title_height, area.width, 1);
            let [_, name, status, location] = columns(header);
            let style = Style::default().add_modifier(Modifier::BOLD);
            frame.render_widget(Paragraph::new("Agent").style(style), name);
            frame.render_widget(Paragraph::new("Status").style(style), status);
            frame.render_widget(
                Paragraph::new("Session / Tab / Pane").style(style),
                location,
            );
        }

        if self.snapshot.entries.is_empty() {
            frame.render_widget(Paragraph::new("No agents with reported status."), body);
        } else {
            for (row, entry) in self
                .snapshot
                .entries
                .iter()
                .skip(self.scroll)
                .take(height)
                .enumerate()
            {
                let row_area = Rect::new(body.x, body.y + row as u16, body.width, 1);
                let selected = self.selected.as_ref() == Some(&entry.id);
                let style = if selected {
                    Style::default().bg(Color::DarkGray).fg(Color::White)
                } else {
                    Style::default()
                };
                frame.render_widget(Block::default().style(style), row_area);
                let [marker, name, status, location] = columns(row_area);
                frame.render_widget(
                    Paragraph::new(if selected { ">" } else { " " }).style(style),
                    marker,
                );
                // Separate clipped columns keep long Unicode names off the status/location.
                frame.render_widget(Paragraph::new(plain(&entry.name)).style(style), name);
                frame.render_widget(
                    Paragraph::new(entry.status.label())
                        .style(style.fg(status_color(entry.status))),
                    status,
                );
                frame.render_widget(
                    Paragraph::new(location_text(entry, location.width)).style(style),
                    location,
                );
                self.hits.push(HitRegion {
                    area: row_area,
                    entry: entry.clone(),
                });
            }
        }

        if footer_height == 2 {
            frame.render_widget(
                Paragraph::new(
                    "↑/↓ select | Enter/click navigate | Wheel scroll | w details | q/Esc/Ctrl-C quit",
                ),
                Rect::new(area.x, area.bottom() - 2, area.width, 1),
            );
        }
        let (status, color) = self.footer();
        frame.render_widget(
            Paragraph::new(status).style(Style::default().fg(color)),
            Rect::new(area.x, area.bottom() - 1, area.width, 1),
        );
    }

    fn toggle_details(&mut self) {
        self.details = !self.details;
        self.details_scroll = 0;
        self.details_limit = 0;
        self.viewport = Rect::default();
        self.hits.clear();
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
        let area = frame.area();
        let title_height = u16::from(area.height >= 3);
        let body = Rect::new(
            area.x,
            area.y + title_height,
            area.width,
            area.height - title_height - 1,
        );
        self.viewport = body;
        let message = report_text(&self.message);
        let mut lines = wrap_details(
            &format!(
                "Message: {}",
                if message.is_empty() {
                    "No current message."
                } else {
                    &message
                }
            ),
            body.width,
        );
        lines.push(Line::raw(""));
        lines.extend(wrap_details(
            &format!("Warnings: {}", self.snapshot.warnings.len()),
            body.width,
        ));
        if self.snapshot.warnings.is_empty() {
            lines.extend(wrap_details("No snapshot warnings.", body.width));
        } else {
            for (index, warning) in self.snapshot.warnings.iter().enumerate() {
                lines.extend(wrap_details(
                    &format!("{}. {}", index + 1, report_text(warning)),
                    body.width,
                ));
            }
        }
        let count = lines.len();
        let height = usize::from(body.height);
        self.details_limit = count.saturating_sub(height);
        self.details_scroll = self.details_scroll.min(self.details_limit);
        if title_height > 0 {
            let first = if height == 0 {
                0
            } else {
                self.details_scroll + 1
            };
            let last = (self.details_scroll + height).min(count);
            frame.render_widget(
                Paragraph::new(format!("brachio | Details | lines {first}-{last}/{count}"))
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
            Paragraph::new("↑/↓/Wheel scroll | Esc/w back | q/Ctrl-C quit"),
            Rect::new(area.x, area.bottom() - 1, area.width, 1),
        );
    }

    fn selected_index(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.snapshot
            .entries
            .iter()
            .position(|entry| &entry.id == selected)
    }

    fn selection_removed(&mut self) {
        self.selected = None;
        self.selection_missing = true;
        self.message =
            "Selected agent is no longer available. Use ↑/↓ or click to select another.".into();
    }

    fn move_selection(&mut self, down: bool) {
        let count = self.snapshot.entries.len();
        if count == 0 {
            self.selected = None;
            self.scroll = 0;
            return;
        }
        let next = match self.selected_index() {
            Some(index) if down => (index + 1).min(count - 1),
            Some(index) => index.saturating_sub(1),
            None if down => 0,
            None => count - 1,
        };
        self.selected = Some(self.snapshot.entries[next].id.clone());
        self.selection_missing = false;
        self.reveal_selection = true;
        self.message.clear();
    }

    fn navigate_selected(&mut self) -> Option<Action> {
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
        self.message = format!("Switching to {}.", plain(&entry.location));
        Some(Action::Navigate(Box::new(entry)))
    }

    fn footer(&self) -> (String, Color) {
        let (state, mut color) = if !self.connected {
            ("STALE: disconnected; navigation disabled", Color::Red)
        } else if self.navigating {
            ("Navigating; please wait", Color::Yellow)
        } else {
            ("Connected", Color::Green)
        };
        let mut text = format!(
            "{state} | Warnings: {} (w details)",
            self.snapshot.warnings.len()
        );
        let message = plain(&self.message);
        if !message.is_empty() {
            text.push_str(" | ");
            text.push_str(&message);
            if self.connected && !self.navigating {
                color = Color::Yellow;
            }
        }
        if !self.snapshot.warnings.is_empty() && self.connected {
            color = Color::Yellow;
        }
        (text, color)
    }
}

// Keep the model's display filter, but not its 256-character row limit.
fn report_text(value: &str) -> String {
    let mut text = String::new();
    let mut remaining = value;
    while !remaining.is_empty() {
        let end = remaining
            .char_indices()
            .nth(256)
            .map_or(remaining.len(), |(index, _)| index);
        text.push_str(&plain(&remaining[..end]));
        remaining = &remaining[end..];
    }
    text
}

fn wrap_details(value: &str, width: u16) -> Vec<Line<'static>> {
    let text = Span::raw(value);
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    for grapheme in text.styled_graphemes(Style::default()) {
        let size = Span::raw(grapheme.symbol).width();
        if used + size > usize::from(width) && !line.is_empty() {
            lines.push(Line::raw(std::mem::take(&mut line)));
            used = 0;
        }
        line.push_str(grapheme.symbol);
        used += size;
    }
    lines.push(Line::raw(line));
    lines
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x && x < area.right() && y >= area.y && y < area.bottom()
}

fn columns(area: Rect) -> [Rect; 4] {
    let marker_width = area.width.min(2);
    let status_width = area.width.saturating_sub(marker_width).min(8);
    let remaining = area.width - marker_width - status_width;
    let name_width = (remaining / 3).min(24);
    let name_x = area.x + marker_width;
    let status_x = name_x + name_width;
    let location_x = status_x + status_width;
    [
        Rect::new(area.x, area.y, marker_width, area.height),
        Rect::new(name_x, area.y, name_width.saturating_sub(1), area.height),
        Rect::new(status_x, area.y, status_width, area.height),
        Rect::new(location_x, area.y, remaining - name_width, area.height),
    ]
}

fn location_text(entry: &Entry, width: u16) -> String {
    let location = plain(&entry.location);
    if !location.is_empty() && Line::raw(location.as_str()).width() <= usize::from(width) {
        return location;
    }
    // Long labels must not push the pane identity out of the row.
    let suffix_width = (usize::from(width).saturating_sub(8) / 3).min(8);
    let suffix = |id: &str| {
        let id = plain(id);
        id.chars()
            .skip(id.chars().count().saturating_sub(suffix_width))
            .collect::<String>()
    };
    format!(
        "s:{} t:{} p:{}",
        suffix(&entry.target.session),
        suffix(&entry.target.window),
        suffix(&entry.target.block),
    )
}

fn status_color(status: Status) -> Color {
    match status {
        Status::Blocked | Status::Error => Color::Red,
        Status::Done => Color::Green,
        Status::Unknown => Color::Magenta,
        Status::Working => Color::Cyan,
        Status::Idle => Color::Gray,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Target;
    use crossterm::event::{KeyEvent, MouseEvent};
    use ratatui::{Terminal, backend::TestBackend};

    fn entry(index: usize, status: Status) -> Entry {
        Entry {
            id: EntryId {
                session: format!("session-{index}"),
                block: format!("pane-{index}"),
                agent: "pi".into(),
            },
            target: Target {
                session: format!("session-{index}"),
                window: format!("tab-{index}"),
                block: format!("pane-{index}"),
            },
            name: format!("agent-{index:02}"),
            status,
            location: format!("session-{index:02} / tab-{index:02} / pane-{index:02}"),
            records: 1,
        }
    }

    fn app(entries: Vec<Entry>) -> App {
        let mut app = App {
            connected: true,
            ..App::default()
        };
        app.replace_snapshot(Snapshot {
            entries,
            warnings: Vec::new(),
        });
        app
    }

    fn many(count: usize) -> App {
        app((0..count).map(|i| entry(i, Status::Working)).collect())
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn click(x: u16, y: u16) -> Event {
        mouse(MouseEventKind::Down(MouseButton::Left), x, y)
    }

    fn terminal(width: u16, height: u16) -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(width, height)).unwrap()
    }

    fn render(app: &mut App, terminal: &mut Terminal<TestBackend>) -> Vec<String> {
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        buffer
            .content
            .chunks(usize::from(buffer.area.width).max(1))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect()
    }

    fn navigation(action: Option<Action>) -> Entry {
        match action {
            Some(Action::Navigate(entry)) => *entry,
            other => panic!("expected navigation, got {other:?}"),
        }
    }

    #[test]
    fn default_and_empty_view_have_status_help_and_no_hit_targets() {
        let mut app = App::default();
        let rows = render(&mut app, &mut terminal(100, 10));
        assert!(rows[0].contains("brachio | 0 agents"));
        assert!(rows[1].contains("Session / Tab / Pane"));
        assert!(rows[2].contains("No agents"));
        assert!(rows[8].contains("Enter/click navigate"));
        assert!(rows[9].contains("STALE: disconnected"));
        assert!(app.hits.is_empty());
        assert!(app.handle_event(click(3, 2)).is_none());
        app.connected = true;
        assert!(app.handle_event(key(KeyCode::Enter)).is_none());
        assert!(app.message.contains("No agent selected"));
        assert!(app.handle_event(key(KeyCode::Down)).is_none());
        assert!(app.handle_event(key(KeyCode::Up)).is_none());
        assert!(app.selected.is_none());
    }

    #[test]
    fn rows_are_grouped_by_model_priority_with_name_status_and_location() {
        let statuses = [
            Status::Idle,
            Status::Working,
            Status::Unknown,
            Status::Done,
            Status::Error,
            Status::Blocked,
        ];
        let mut app = app(statuses
            .into_iter()
            .enumerate()
            .map(|(i, status)| entry(i, status))
            .collect());
        let rows = render(&mut app, &mut terminal(100, 12));
        for (i, entry) in app.snapshot.entries.iter().enumerate() {
            assert!(rows[i + 2].contains(&entry.name));
            assert!(rows[i + 2].contains(entry.status.label()));
            assert!(rows[i + 2].contains(&entry.location));
        }
        assert_eq!(app.snapshot.entries[0].status, Status::Blocked);
        assert_eq!(app.snapshot.entries[5].status, Status::Idle);
        assert_eq!(app.selected, Some(app.snapshot.entries[0].id.clone()));
    }

    #[test]
    fn equal_statuses_use_the_same_location_and_identity_order_as_the_model() {
        let mut a = entry(1, Status::Working);
        let mut b = entry(2, Status::Working);
        a.location = "same location".into();
        b.location = a.location.clone();
        let mut expected = Snapshot {
            entries: vec![b, entry(3, Status::Blocked), a],
            warnings: Vec::new(),
        };
        expected.sort();
        let app = app(expected.entries.iter().cloned().rev().collect());
        assert_eq!(app.snapshot.entries, expected.entries);
    }

    #[test]
    fn long_unicode_names_are_clipped_without_covering_status_or_location() {
        let mut agent = entry(0, Status::Blocked);
        agent.name = format!("\x1b\n\u{202e}{}", "界e\u{301}🦕".repeat(100));
        let mut app = app(vec![agent]);
        app.message = "failure\x1b\n\u{202e}: denied".into();
        app.snapshot.warnings.push("warn\r\u{2066}ing".into());
        let mut terminal = terminal(100, 8);
        let rows = render(&mut app, &mut terminal);
        assert!(rows[2].contains("Blocked"));
        assert!(rows[2].contains("session-00 / tab-00 / pane-00"));
        assert!(!rows[3].contains('界'));
        assert!(rows[7].contains("failure: denied"));
        assert!(rows[7].contains("Warnings: 1 (w details)"));
        for cell in &terminal.backend().buffer().content {
            assert_eq!(plain(cell.symbol()), cell.symbol());
        }
    }

    #[test]
    fn long_location_labels_keep_session_tab_and_pane_identities_in_the_row() {
        let mut agent = entry(0, Status::Working);
        agent.location = format!(
            "{} / {} / {}",
            "session".repeat(8),
            "tab".repeat(8),
            "pane".repeat(8)
        );
        agent.target = Target {
            session: "session:00000000-0000-0000-0000-000000000001".into(),
            window: "window:00000000-0000-0000-0000-000000000002".into(),
            block: "block:00000000-0000-0000-0000-000000000003".into(),
        };
        let mut app = app(vec![agent]);
        let rows = render(&mut app, &mut terminal(80, 8));
        assert!(rows[2].contains("s:00000001 t:00000002 p:00000003"));
        let rows = render(&mut app, &mut terminal(50, 8));
        assert!(rows[2].contains("s:000001 t:000002 p:000003"));
    }

    #[test]
    fn all_tiny_dimensions_render_without_overflow_or_hidden_hit_regions() {
        for width in [0, 1, 2, 8, 17, 80] {
            for height in [0, 1, 2, 3, 4, 5, 24] {
                let mut app = many(3);
                let mut terminal = terminal(width, height);
                render(&mut app, &mut terminal);
                for hit in &app.hits {
                    assert!(hit.area.width > 0);
                    assert_eq!(hit.area.height, 1);
                    assert!(hit.area.right() <= width);
                    assert!(hit.area.bottom() <= height.saturating_sub(2));
                }
                if width == 0 || height <= 2 {
                    assert!(app.hits.is_empty());
                }
            }
        }
    }

    #[test]
    fn arrow_selection_is_bounded_and_enter_returns_the_full_entry() {
        let mut app = many(3);
        app.handle_event(key(KeyCode::Up));
        assert_eq!(app.selected, Some(entry(0, Status::Working).id));
        for _ in 0..10 {
            app.handle_event(key(KeyCode::Down));
        }
        assert_eq!(app.selected, Some(entry(2, Status::Working).id));
        let target = navigation(app.handle_event(key(KeyCode::Enter)));
        assert_eq!(target, entry(2, Status::Working));
        assert!(app.navigating);
    }

    #[test]
    fn arrows_scroll_to_keep_the_selected_entry_visible() {
        let mut app = many(20);
        let mut terminal = terminal(80, 8);
        render(&mut app, &mut terminal);
        for _ in 0..12 {
            app.handle_event(key(KeyCode::Down));
        }
        let rows = render(&mut app, &mut terminal);
        assert_eq!(app.scroll, 9);
        assert_eq!(app.hits.len(), 4);
        assert_eq!(app.hits[3].entry, entry(12, Status::Working));
        assert!(rows[5].starts_with('>'));
        app.handle_event(key(KeyCode::Up));
        render(&mut app, &mut terminal);
        assert_eq!(app.scroll, 9);
        for _ in 0..10 {
            app.handle_event(key(KeyCode::Up));
        }
        render(&mut app, &mut terminal);
        assert_eq!(app.scroll, 1);
    }

    #[test]
    fn selection_follows_identity_through_status_name_and_location_changes() {
        let mut app = many(2);
        app.handle_event(key(KeyCode::Down));
        let mut selected = entry(1, Status::Blocked);
        selected.name = "renamed".into();
        selected.location = "new session / new tab / pane-1".into();
        selected.target.window = "new-tab-1".into();
        selected.records = 4;
        app.replace_snapshot(Snapshot {
            entries: vec![entry(0, Status::Done), selected.clone()],
            warnings: Vec::new(),
        });
        assert_eq!(app.selected, Some(selected.id.clone()));
        assert_eq!(app.selected_index(), Some(0));
        assert_eq!(navigation(app.handle_event(key(KeyCode::Enter))), selected);
    }

    #[test]
    fn removed_selection_stays_empty_across_draws_and_future_snapshots() {
        let mut app = many(2);
        app.handle_event(key(KeyCode::Down));
        app.replace_snapshot(Snapshot {
            entries: vec![entry(0, Status::Working)],
            warnings: Vec::new(),
        });
        assert!(app.selected.is_none());
        assert!(app.message.contains("no longer available"));
        let mut terminal = terminal(100, 8);
        let rows = render(&mut app, &mut terminal);
        assert!(!rows[2].starts_with('>'));
        app.replace_snapshot(Snapshot {
            entries: vec![entry(0, Status::Working), entry(2, Status::Blocked)],
            warnings: Vec::new(),
        });
        render(&mut app, &mut terminal);
        assert!(app.selected.is_none());
        assert!(app.handle_event(key(KeyCode::Enter)).is_none());
        assert!(!app.navigating);
        app.handle_event(key(KeyCode::Down));
        assert_eq!(
            navigation(app.handle_event(key(KeyCode::Enter))),
            entry(2, Status::Blocked)
        );
    }

    #[test]
    fn a_same_name_replacement_does_not_inherit_the_removed_selection() {
        let old = entry(0, Status::Working);
        let mut replacement = entry(1, Status::Working);
        replacement.name = old.name.clone();
        replacement.location = old.location.clone();
        let mut app = app(vec![old]);
        app.replace_snapshot(Snapshot {
            entries: vec![replacement.clone()],
            warnings: Vec::new(),
        });
        render(&mut app, &mut terminal(80, 8));
        assert!(app.selected.is_none());
        assert!(app.handle_event(key(KeyCode::Enter)).is_none());
        app.handle_event(key(KeyCode::Down));
        assert_eq!(
            navigation(app.handle_event(key(KeyCode::Enter))),
            replacement
        );
    }

    #[test]
    fn an_initial_empty_snapshot_does_not_select_a_later_unseen_entry() {
        let mut app = app(Vec::new());
        app.replace_snapshot(Snapshot {
            entries: vec![entry(0, Status::Working)],
            warnings: Vec::new(),
        });
        assert!(app.selected.is_none());
        app.handle_event(key(KeyCode::Up));
        assert_eq!(app.selected, Some(entry(0, Status::Working).id));
    }

    #[test]
    fn removing_all_entries_then_repopulating_needs_a_deliberate_selection() {
        let mut app = many(1);
        app.replace_snapshot(Snapshot::default());
        assert!(app.selected.is_none());
        app.replace_snapshot(Snapshot {
            entries: vec![entry(1, Status::Working)],
            warnings: Vec::new(),
        });
        let mut terminal = terminal(80, 8);
        render(&mut app, &mut terminal);
        app.handle_event(mouse(MouseEventKind::ScrollDown, 0, 2));
        assert!(app.selected.is_none());
        render(&mut app, &mut terminal);
        assert_eq!(
            navigation(app.handle_event(click(0, 2))),
            entry(1, Status::Working)
        );
    }

    #[test]
    fn enter_and_click_navigate_the_same_exact_entry_not_a_name_match() {
        let mut entries = vec![entry(0, Status::Working), entry(1, Status::Working)];
        entries[0].name = "pi".into();
        entries[1].name = "pi".into();
        let mut keyboard = app(entries.clone());
        keyboard.handle_event(key(KeyCode::Down));
        let expected = navigation(keyboard.handle_event(key(KeyCode::Enter)));
        let mut pointer = app(entries);
        render(&mut pointer, &mut terminal(80, 8));
        let actual = navigation(pointer.handle_event(click(79, 3)));
        assert_eq!(actual, expected);
        assert_eq!(pointer.selected, Some(actual.id));
    }

    #[test]
    fn click_requires_a_draw_and_only_drawn_entry_rows_are_targets() {
        let mut app = many(1);
        assert!(app.handle_event(click(0, 2)).is_none());
        let mut terminal = terminal(80, 8);
        for (x, y) in [(0, 0), (0, 1), (0, 3), (0, 6), (0, 7), (80, 2), (0, 8)] {
            render(&mut app, &mut terminal);
            assert!(app.handle_event(click(x, y)).is_none());
            assert!(!app.navigating);
        }
        render(&mut app, &mut terminal);
        assert!(
            app.handle_event(mouse(MouseEventKind::Down(MouseButton::Right), 0, 2))
                .is_none()
        );
        render(&mut app, &mut terminal);
        assert_eq!(
            navigation(app.handle_event(click(0, 2))),
            entry(0, Status::Working)
        );
    }

    #[test]
    fn snapshot_replacement_invalidates_even_an_unchanged_hit_map() {
        let mut app = many(2);
        let mut terminal = terminal(80, 8);
        render(&mut app, &mut terminal);
        app.replace_snapshot(app.snapshot.clone());
        assert!(app.handle_event(click(0, 2)).is_none());
        render(&mut app, &mut terminal);
        app.replace_snapshot(Snapshot {
            entries: vec![entry(1, Status::Blocked), entry(0, Status::Working)],
            warnings: Vec::new(),
        });
        assert!(app.handle_event(click(0, 2)).is_none());
        render(&mut app, &mut terminal);
        assert_eq!(
            navigation(app.handle_event(click(0, 2))),
            entry(1, Status::Blocked)
        );
    }

    #[test]
    fn removed_rows_cannot_be_clicked_before_the_next_draw() {
        let mut app = many(2);
        let mut terminal = terminal(80, 8);
        render(&mut app, &mut terminal);
        app.replace_snapshot(Snapshot {
            entries: vec![entry(1, Status::Working)],
            warnings: Vec::new(),
        });
        assert!(app.handle_event(click(0, 2)).is_none());
        assert!(app.selected.is_none());
        assert!(app.message.contains("no longer available"));
        render(&mut app, &mut terminal);
        assert_eq!(
            navigation(app.handle_event(click(0, 2))),
            entry(1, Status::Working)
        );
    }

    #[test]
    fn non_mutating_events_preserve_a_following_click_in_the_same_input_batch() {
        let mut app = many(2);
        render(&mut app, &mut terminal(80, 8));
        for event in [
            mouse(MouseEventKind::Moved, 1, 3),
            mouse(MouseEventKind::Up(MouseButton::Left), 1, 3),
            mouse(MouseEventKind::Drag(MouseButton::Left), 1, 3),
            Event::Key(KeyEvent::new_with_kind(
                KeyCode::Down,
                KeyModifiers::NONE,
                KeyEventKind::Release,
            )),
            Event::FocusGained,
            Event::Paste("ignored".into()),
        ] {
            assert!(app.handle_event(event).is_none());
            assert_eq!(app.hits.len(), 2);
        }
        assert_eq!(
            navigation(app.handle_event(click(1, 3))),
            entry(1, Status::Working)
        );
    }

    #[test]
    fn keyboard_input_invalidates_click_regions_until_the_next_draw() {
        let mut app = many(2);
        let mut terminal = terminal(80, 8);
        render(&mut app, &mut terminal);
        app.handle_event(key(KeyCode::Down));
        assert!(app.handle_event(click(0, 2)).is_none());
        assert_eq!(app.selected, Some(entry(1, Status::Working).id));
        render(&mut app, &mut terminal);
        assert_eq!(
            navigation(app.handle_event(click(0, 2))),
            entry(0, Status::Working)
        );
    }

    #[test]
    fn wheel_scrolls_the_view_and_click_uses_the_drawn_offset() {
        let mut app = many(15);
        let mut terminal = terminal(80, 8);
        render(&mut app, &mut terminal);
        app.handle_event(mouse(MouseEventKind::ScrollDown, 0, 2));
        assert_eq!(app.scroll, 3);
        assert_eq!(app.selected, Some(entry(0, Status::Working).id));
        assert!(app.handle_event(click(0, 2)).is_none());
        render(&mut app, &mut terminal);
        assert_eq!(app.hits[0].entry, entry(3, Status::Working));
        assert_eq!(
            navigation(app.handle_event(click(0, 2))),
            entry(3, Status::Working)
        );
    }

    #[test]
    fn wheel_is_bounded_and_does_not_scroll_from_a_footer() {
        let mut app = many(15);
        let mut terminal = terminal(80, 8);
        render(&mut app, &mut terminal);
        app.handle_event(mouse(MouseEventKind::ScrollDown, 0, 7));
        assert_eq!(app.scroll, 0);
        for _ in 0..20 {
            app.handle_event(mouse(MouseEventKind::ScrollDown, 0, 2));
        }
        assert_eq!(app.scroll, 11);
        render(&mut app, &mut terminal);
        assert_eq!(app.hits.last().unwrap().entry, entry(14, Status::Working));
        for _ in 0..20 {
            app.handle_event(mouse(MouseEventKind::ScrollUp, 0, 2));
        }
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn unchanged_refresh_preserves_manual_scroll_and_missing_selection_notice() {
        let mut app = many(15);
        let mut terminal = terminal(80, 8);
        render(&mut app, &mut terminal);
        app.handle_event(mouse(MouseEventKind::ScrollDown, 0, 2));
        render(&mut app, &mut terminal);
        let scroll = app.scroll;
        app.replace_snapshot(app.snapshot.clone());
        render(&mut app, &mut terminal);
        assert_eq!(app.scroll, scroll);
        let mut snapshot = app.snapshot.clone();
        snapshot.entries.remove(0);
        app.replace_snapshot(snapshot.clone());
        app.message = "Previous app selection accepted".into();
        app.replace_snapshot(snapshot);
        assert!(app.message.contains("no longer available"));
        assert!(app.selected.is_none());
    }

    #[test]
    fn wheel_does_not_change_the_target_of_enter() {
        let mut app = many(15);
        render(&mut app, &mut terminal(80, 8));
        app.handle_event(mouse(MouseEventKind::ScrollDown, 0, 2));
        assert_eq!(
            navigation(app.handle_event(key(KeyCode::Enter))),
            entry(0, Status::Working)
        );
    }

    #[test]
    fn resize_invalidates_coordinates_and_reveals_the_same_selection() {
        let mut app = many(20);
        let mut terminal = terminal(100, 12);
        for _ in 0..12 {
            app.handle_event(key(KeyCode::Down));
        }
        render(&mut app, &mut terminal);
        app.handle_event(Event::Resize(30, 5));
        assert!(app.handle_event(click(0, 3)).is_none());
        terminal.backend_mut().resize(30, 5);
        terminal.resize(Rect::new(0, 0, 30, 5)).unwrap();
        render(&mut app, &mut terminal);
        assert_eq!(app.hits.len(), 1);
        assert_eq!(app.hits[0].entry, entry(12, Status::Working));
        assert_eq!(
            navigation(app.handle_event(click(29, 2))),
            entry(12, Status::Working)
        );
    }

    #[test]
    fn disconnected_and_in_progress_navigation_are_rejected_for_keys_and_mouse() {
        let mut app = many(2);
        app.connected = false;
        assert!(app.handle_event(key(KeyCode::Enter)).is_none());
        assert!(!app.navigating);
        assert!(app.message.contains("disconnected"));
        let mut terminal = terminal(100, 8);
        render(&mut app, &mut terminal);
        assert!(app.handle_event(click(0, 3)).is_none());
        assert!(!app.navigating);
        app.connected = true;
        let target = navigation(app.handle_event(key(KeyCode::Enter)));
        assert_eq!(target, entry(1, Status::Working));
        assert!(app.navigating);
        assert!(app.handle_event(key(KeyCode::Enter)).is_none());
        assert!(app.message.contains("already in progress"));
        let rows = render(&mut app, &mut terminal);
        assert!(rows[7].contains("Navigating; please wait"));
        assert!(app.handle_event(click(0, 2)).is_none());
        app.navigating = false;
        assert_eq!(
            navigation(app.handle_event(key(KeyCode::Enter))),
            entry(0, Status::Working)
        );
    }

    #[test]
    fn errors_and_partial_snapshot_warnings_remain_visible() {
        let mut app = many(1);
        app.message = "Navigation failed: Remote Control denied".into();
        app.snapshot.warnings = vec!["Pane status unavailable".into(), "Other warning".into()];
        let mut terminal = terminal(160, 8);
        let rows = render(&mut app, &mut terminal);
        assert!(rows[7].contains("Navigation failed: Remote Control denied"));
        assert!(rows[7].contains("Warnings: 2 (w details)"));
        app.connected = false;
        let rows = render(&mut app, &mut terminal);
        assert!(rows[7].contains("STALE: disconnected"));
        assert!(rows[7].contains("Navigation failed"));
        assert!(rows[2].contains("agent-00"));
    }

    #[test]
    fn warning_access_precedes_long_client_notices_and_errors_at_eighty_columns() {
        let mut app = many(1);
        let message = "Target app client:00000000-0000-0000-0000-000000000001; session focus is shared with other clients";
        app.message = message.into();
        app.snapshot.warnings = vec!["First warning".into(), "Second warning".into()];
        let mut terminal = terminal(80, 8);
        let rows = render(&mut app, &mut terminal);
        assert!(rows[6].contains("w details"));
        assert!(rows[6].contains("q/Esc/Ctrl-C quit"));
        assert!(rows[7].contains("Warnings: 2 (w details)"));
        assert!(rows[7].contains("Target app client:"));
        assert!(rows[7].find("Warnings:").unwrap() < rows[7].find("Target app").unwrap());
        assert_eq!(app.message, message);

        app.message = "Navigation failed: Remote Control denied".into();
        let rows = render(&mut app, &mut terminal);
        assert!(rows[7].contains("Warnings: 2 (w details)"));
        assert!(rows[7].contains("Navigation failed: Remote Control denied"));
        app.connected = false;
        let rows = render(&mut app, &mut terminal);
        assert!(rows[7].contains("STALE: disconnected"));
        assert!(rows[7].contains("Warnings: 2 (w details)"));
        assert!(rows[7].contains("Navigation"));
    }

    #[test]
    fn details_scroll_exposes_the_full_sanitized_message_and_every_warning() {
        let mut app = many(2);
        app.message = format!(
            "Navigation failed: {} \x1b\n\u{202e}MESSAGE-END",
            "界e\u{301}🦕".repeat(100)
        );
        app.snapshot.warnings = (0..8)
            .map(|index| {
                format!(
                    "WARNING-{index}: {} \r\u{2066}WARNING-END-{index}",
                    "pane status unavailable; ".repeat(20)
                )
            })
            .collect();
        let message = app.message.clone();
        let warnings = app.snapshot.warnings.clone();
        let selected = app.selected.clone();
        let mut terminal = terminal(80, 8);
        assert!(app.handle_event(key(KeyCode::Char('w'))).is_none());
        let rows = render(&mut app, &mut terminal);
        assert!(rows[0].contains("Details | lines"));
        assert!(rows[1].contains("Message: Navigation failed:"));
        assert!(rows[7].contains("Esc/w back | q/Ctrl-C quit"));
        let mut text: String = rows[1..7].iter().map(|row| row.trim_end()).collect();
        assert!(!text.contains("WARNING-END-7"));
        app.handle_event(key(KeyCode::Up));
        assert_eq!(app.details_scroll, 0);
        app.handle_event(mouse(MouseEventKind::ScrollDown, 0, 7));
        assert_eq!(app.details_scroll, 0);
        app.handle_event(mouse(MouseEventKind::ScrollDown, 0, 1));
        assert_eq!(app.details_scroll, 3);
        app.handle_event(mouse(MouseEventKind::ScrollUp, 0, 1));
        assert_eq!(app.details_scroll, 0);

        let limit = app.details_limit;
        for _ in 0..limit {
            app.handle_event(key(KeyCode::Down));
            let rows = render(&mut app, &mut terminal);
            text.push_str(rows[6].trim_end());
            for cell in &terminal.backend().buffer().content {
                assert_eq!(plain(cell.symbol()), cell.symbol());
            }
        }
        assert!(text.contains("MESSAGE-END"));
        assert!(text.contains("Warnings: 8"));
        for index in 0..8 {
            assert!(text.contains(&format!("WARNING-{index}:")));
            assert!(text.contains(&format!("WARNING-END-{index}")));
        }
        app.handle_event(key(KeyCode::Down));
        app.handle_event(mouse(MouseEventKind::ScrollDown, 0, 1));
        assert_eq!(app.details_scroll, limit);
        assert_eq!(app.message, message);
        assert_eq!(app.snapshot.warnings, warnings);
        assert_eq!(app.selected, selected);
        assert!(!app.navigating);
    }

    #[test]
    fn details_never_navigates_on_enter_or_click_and_toggles_invalidate_hits() {
        let mut app = many(3);
        app.handle_event(key(KeyCode::Down));
        app.message = "Navigation failed: denied".into();
        let selected = app.selected.clone();
        let mut terminal = terminal(80, 8);
        render(&mut app, &mut terminal);
        assert_eq!(app.hits.len(), 3);
        app.handle_event(key(KeyCode::Char('w')));
        assert!(app.hits.is_empty());
        assert!(app.handle_event(click(0, 2)).is_none());
        render(&mut app, &mut terminal);
        assert!(app.hits.is_empty());
        for event in [
            key(KeyCode::Enter),
            Event::Key(KeyEvent::new_with_kind(
                KeyCode::Enter,
                KeyModifiers::NONE,
                KeyEventKind::Repeat,
            )),
            click(0, 2),
            click(79, 3),
            click(0, 7),
            mouse(MouseEventKind::Down(MouseButton::Right), 0, 2),
        ] {
            assert!(app.handle_event(event).is_none());
            assert_eq!(app.selected, selected);
            assert!(!app.navigating);
            assert!(app.hits.is_empty());
            assert_eq!(app.message, "Navigation failed: denied");
        }
        app.handle_event(key(KeyCode::Char('w')));
        assert!(!app.details);
        assert!(app.handle_event(click(0, 2)).is_none());
        render(&mut app, &mut terminal);
        assert_eq!(
            navigation(app.handle_event(key(KeyCode::Enter))),
            entry(1, Status::Working)
        );
    }

    #[test]
    fn closing_details_preserves_the_stable_target_and_manual_list_scroll() {
        let mut app = many(20);
        for _ in 0..5 {
            app.handle_event(key(KeyCode::Down));
        }
        let mut terminal = terminal(80, 8);
        render(&mut app, &mut terminal);
        for _ in 0..2 {
            app.handle_event(mouse(MouseEventKind::ScrollDown, 0, 2));
        }
        render(&mut app, &mut terminal);
        let scroll = app.scroll;
        let selected = app.selected.clone();
        assert!(
            !app.hits
                .iter()
                .any(|hit| Some(&hit.entry.id) == selected.as_ref())
        );
        app.message = "Full message. ".repeat(80);
        app.handle_event(key(KeyCode::Char('w')));
        render(&mut app, &mut terminal);
        app.handle_event(key(KeyCode::Down));
        app.replace_snapshot(app.snapshot.clone());
        render(&mut app, &mut terminal);
        assert!(app.details_scroll > 0);
        assert!(app.handle_event(key(KeyCode::Esc)).is_none());
        assert!(!app.details);
        assert!(app.handle_event(click(0, 2)).is_none());
        render(&mut app, &mut terminal);
        assert_eq!(app.scroll, scroll);
        assert_eq!(app.selected, selected);
        assert_eq!(
            navigation(app.handle_event(key(KeyCode::Enter))),
            entry(5, Status::Working)
        );
    }

    #[test]
    fn selection_removed_during_details_still_requires_deliberate_reselection() {
        let mut app = many(2);
        app.handle_event(key(KeyCode::Down));
        app.handle_event(key(KeyCode::Char('w')));
        app.replace_snapshot(Snapshot {
            entries: vec![entry(0, Status::Working)],
            warnings: Vec::new(),
        });
        let mut terminal = terminal(80, 8);
        let rows = render(&mut app, &mut terminal);
        assert!(rows[1].contains("Selected agent is no longer available"));
        app.handle_event(key(KeyCode::Char('w')));
        render(&mut app, &mut terminal);
        assert!(app.handle_event(key(KeyCode::Enter)).is_none());
        assert!(app.selected.is_none());
        assert!(!app.navigating);
        app.handle_event(key(KeyCode::Down));
        assert_eq!(
            navigation(app.handle_event(key(KeyCode::Enter))),
            entry(0, Status::Working)
        );
    }

    #[test]
    fn details_resize_and_empty_reports_clamp_scroll_without_creating_targets() {
        let mut app = many(1);
        app.snapshot.warnings = vec!["Long warning. ".repeat(80)];
        let selected = app.selected.clone();
        let mut terminal = terminal(80, 8);
        render(&mut app, &mut terminal);
        app.handle_event(key(KeyCode::Char('w')));
        render(&mut app, &mut terminal);
        for _ in 0..100 {
            app.handle_event(key(KeyCode::Down));
        }
        assert!(app.details_scroll > 0);
        app.handle_event(Event::Resize(30, 5));
        assert!(app.hits.is_empty());
        assert_eq!(app.viewport, Rect::default());
        assert!(app.handle_event(click(0, 2)).is_none());
        terminal.backend_mut().resize(30, 5);
        terminal.resize(Rect::new(0, 0, 30, 5)).unwrap();
        let rows = render(&mut app, &mut terminal);
        assert!(rows[0].contains("Details"));
        assert_eq!(app.viewport, Rect::new(0, 1, 30, 3));
        assert!(app.details_scroll <= app.details_limit);

        app.message.clear();
        app.replace_snapshot(Snapshot {
            entries: app.snapshot.entries.clone(),
            warnings: Vec::new(),
        });
        app.handle_event(Event::Resize(80, 8));
        terminal.backend_mut().resize(80, 8);
        terminal.resize(Rect::new(0, 0, 80, 8)).unwrap();
        let rows = render(&mut app, &mut terminal);
        assert!(rows[1].contains("Message: No current message."));
        assert!(rows[3].contains("Warnings: 0"));
        assert!(rows[4].contains("No snapshot warnings."));
        assert_eq!(app.details_scroll, 0);
        assert_eq!(app.details_limit, 0);
        assert_eq!(app.selected, selected);
        assert!(app.hits.is_empty());
        assert!(!app.navigating);
    }

    #[test]
    fn tiny_details_views_have_no_overflow_or_agent_hit_regions() {
        for width in [0, 1, 2, 8, 17, 80] {
            for height in [0, 1, 2, 3, 4, 5, 24] {
                let mut app = many(3);
                app.snapshot.warnings = vec!["界e\u{301}🦕".repeat(100)];
                app.handle_event(key(KeyCode::Char('w')));
                render(&mut app, &mut terminal(width, height));
                assert!(app.details);
                assert!(app.hits.is_empty());
                assert!(app.handle_event(click(0, 0)).is_none());
                assert!(app.handle_event(key(KeyCode::Enter)).is_none());
                assert!(!app.navigating);
            }
        }
    }

    #[test]
    fn details_return_and_quit_keys_ignore_key_releases() {
        let mut app = many(1);
        let mut terminal = terminal(80, 8);
        render(&mut app, &mut terminal);
        let release = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('w'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        assert!(app.handle_event(release.clone()).is_none());
        assert!(!app.details);
        assert_eq!(app.hits.len(), 1);
        app.handle_event(key(KeyCode::Char('w')));
        assert!(app.handle_event(release).is_none());
        assert!(app.details);
        assert!(app.handle_event(key(KeyCode::Esc)).is_none());
        assert!(!app.details);
        app.handle_event(key(KeyCode::Char('w')));
        app.connected = false;
        app.navigating = true;
        for event in [
            key(KeyCode::Char('q')),
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Event::Key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::CONTROL)),
        ] {
            assert!(matches!(app.handle_event(event), Some(Action::Quit)));
        }
    }

    #[test]
    fn direct_public_snapshot_mutation_cannot_retarget_an_old_click() {
        let mut app = many(2);
        render(&mut app, &mut terminal(80, 8));
        app.snapshot.entries[0].target.window = "different-tab".into();
        assert!(app.handle_event(click(0, 2)).is_none());
        assert!(app.message.contains("list changed"));
        assert!(!app.navigating);
        app.snapshot.entries.remove(0);
        assert!(app.handle_event(key(KeyCode::Enter)).is_none());
        assert!(app.selected.is_none());
        assert!(app.message.contains("no longer available"));
    }

    #[test]
    fn quit_keys_work_while_offline_or_navigating_and_releases_are_ignored() {
        let mut app = App {
            navigating: true,
            ..App::default()
        };
        for event in [
            key(KeyCode::Char('q')),
            key(KeyCode::Esc),
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Event::Key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::CONTROL)),
        ] {
            assert!(matches!(app.handle_event(event), Some(Action::Quit)));
        }
        assert!(app.handle_event(key(KeyCode::Char('c'))).is_none());
        assert!(
            app.handle_event(Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            )))
            .is_none()
        );
    }
}
