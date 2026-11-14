use super::*;
use crate::model::{Location, Target, plain};
use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, text::Span};
use std::cmp::Ordering;

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
        location: Location {
            session: format!("session-{index:02}").into(),
            tab: format!("tab-{index:02}").into(),
            pane: format!("pane-{index:02}").into(),
        },
        working_dir: None,
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
    let frame = terminal.draw(|frame| app.draw(frame)).unwrap();
    let buffer = frame.buffer;
    buffer
        .content
        .chunks(usize::from(buffer.area.width).max(1))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect())
        .collect()
}

const VIEW_SIZES: [(u16, u16); 5] = [(120, 30), (100, 25), (80, 24), (60, 20), (40, 15)];

fn app_columns(app: &App, area: Rect) -> presentation::ColumnLayout {
    let matching = matching_indices(&app.snapshot, &app.query);
    let home = std::env::var("HOME").ok();
    let measured = measure_columns(
        matching.iter().map(|&index| &app.snapshot.entries[index]),
        home.as_deref(),
    );
    let mut hidden = [false; 6];
    if app.current_groups().len() >= 2 {
        match app.group_by {
            GroupBy::None => {}
            GroupBy::Status => hidden[0] = true,
            GroupBy::Agent => hidden[1] = true,
            GroupBy::Cwd => hidden[2] = true,
            GroupBy::Session => hidden[3] = true,
        }
    }
    columns(area, measured, hidden)
}

// Compare display cells, not UTF-8 byte offsets or wide-character placeholders.
fn assert_cell_text(buffer: &Buffer, area: Rect, value: &str, keep_end: bool) {
    let clipped = clip_text(value, area.width, keep_end);
    assert!(Line::raw(clipped.as_str()).width() <= usize::from(area.width));
    let span = Span::raw(clipped.as_str());
    let mut x = area.x;
    for grapheme in span.styled_graphemes(Style::default()) {
        let width = Span::raw(grapheme.symbol).width() as u16;
        assert!(x + width <= area.right(), "{area:?}: {clipped:?}");
        assert_eq!(buffer[(x, area.y)].symbol(), grapheme.symbol, "{area:?}");
        x += width;
    }
    for x in x..area.right() {
        assert_eq!(buffer[(x, area.y)].symbol(), " ", "{area:?}");
    }
}

fn assert_independent_fields(app: &App, terminal: &Terminal<TestBackend>) {
    let buffer = terminal.backend().buffer();
    let home = std::env::var("HOME").ok();
    for hit in &app.hits {
        let layout = app_columns(app, hit.area);
        let mut right = layout.areas[0].right();
        let mut first = true;
        for (index, area) in layout.areas.iter().copied().enumerate().skip(1) {
            assert!(area.right() <= hit.area.right());
            if area.width == 0 {
                continue;
            }
            assert_eq!(area.x, right + if first { 0 } else { 2 });
            if !first {
                for x in right..area.x {
                    assert_eq!(buffer[(x, area.y)].symbol(), " ");
                }
            }
            first = false;
            right = area.right();
            assert_cell_text(
                buffer,
                Rect { y: 1, ..area },
                COLUMN_LABELS[index - 1],
                false,
            );
        }
        let entry = &hit.entry;
        let cwd = home_path(
            entry.working_dir.as_deref().unwrap_or("Unknown"),
            home.as_deref(),
        );
        let values = [
            status_text(entry.status, layout.compact_status),
            entry.name.as_str(),
            cwd.as_str(),
            entry.location.session.as_ref(),
            entry.location.tab.as_ref(),
            entry.location.pane.as_ref(),
        ];
        for (index, value) in values.into_iter().enumerate() {
            assert_cell_text(buffer, layout.areas[index + 1], value, index == 2);
        }
        if app.selected.as_ref() == Some(&entry.id) {
            let mut x = hit.area.x;
            while x < hit.area.right() {
                let cell = &buffer[(x, hit.area.y)];
                assert_eq!(
                    cell.bg,
                    Color::DarkGray,
                    "selected cell ({x}, {})",
                    hit.area.y
                );
                // Ratatui resets covered continuation cells. The leading grapheme supplies their style.
                x += Span::raw(cell.symbol()).width().max(1) as u16;
            }
        }
    }
}

fn agent_area(app: &App, id: &EntryId) -> Rect {
    app.hits
        .iter()
        .find(|hit| &hit.entry.id == id)
        .expect("agent must be drawn")
        .area
}

fn click_agent(app: &mut App, id: &EntryId) -> Option<Action> {
    let area = agent_area(app, id);
    app.handle_event(click(area.right() - 1, area.y))
}

fn wheel(app: &mut App, down: bool) {
    let area = app.viewport;
    app.handle_event(mouse(
        if down {
            MouseEventKind::ScrollDown
        } else {
            MouseEventKind::ScrollUp
        },
        area.x,
        area.y,
    ));
}

fn assert_no_hits(app: &App) {
    assert!(app.hits.is_empty());
    assert!(app.group_hits.is_empty());
}

// Read all overlay lines through the same scrolling path as the user.
fn overlay_text(app: &mut App, terminal: &mut Terminal<TestBackend>) -> String {
    for _ in 0..app.details_scroll {
        app.handle_event(key(KeyCode::Up));
    }
    let rows = render(app, terminal);
    let body = app.viewport;
    assert!(body.height > 0);
    let mut text: String = rows[usize::from(body.y)..usize::from(body.bottom())]
        .iter()
        .map(|row| row.trim_end())
        .collect();
    let limit = app.details_limit;
    for _ in 0..limit {
        app.handle_event(key(KeyCode::Down));
        let rows = render(app, terminal);
        text.push_str(rows[usize::from(body.bottom() - 1)].trim_end());
    }
    assert_eq!(app.details_scroll, limit);
    text
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
    assert!(rows[0].starts_with("brachio | 0 agents"));
    assert!(!rows[0].contains("Group by"));
    assert!(rows[0].contains("0 agents"));
    let labels = COLUMN_LABELS;
    assert!(!rows[1].contains("Location"));
    for label in labels {
        assert!(rows[1].contains(label));
    }
    assert!(
        labels
            .windows(2)
            .all(|pair| rows[1].find(pair[0]) < rows[1].find(pair[1]))
    );
    assert!(rows[3].contains("No agents"));
    assert!(rows[8].contains("Enter Jump"));
    assert!(rows[8].contains("g/G Group"));
    assert!(rows[9].contains("Disconnected; navigation disabled"));
    assert!(!rows[9].contains("0 warnings"));
    assert_eq!(app.group_by, GroupBy::None);
    assert!(app.group_hits.is_empty());
    assert!(app.hits.is_empty());
    assert!(app.handle_event(click(3, 3)).is_none());
    app.connected = true;
    assert!(app.handle_event(key(KeyCode::Enter)).is_none());
    assert!(app.message.contains("No agent selected"));
    assert!(app.handle_event(key(KeyCode::Down)).is_none());
    assert!(app.handle_event(key(KeyCode::Up)).is_none());
    assert!(app.selected.is_none());
}

fn grouping_entries() -> Vec<Entry> {
    [
        ("pi", Status::Idle, Some("/work/b"), "Beta", "beta"),
        ("claude-code", Status::Working, None, "Alpha", "alpha-1"),
        ("codex", Status::Error, Some("/work/a"), "Alpha", "alpha-1"),
        ("pi", Status::Blocked, Some("/work/b"), "Alpha", "alpha-2"),
        ("claude-code", Status::Done, Some("/work/a"), "Beta", "beta"),
        ("codex", Status::Idle, None, "Alpha", "alpha-2"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (agent, status, cwd, session_name, session_id))| {
        let mut entry = entry(index, status);
        entry.id.agent = agent.into();
        entry.id.session = session_id.into();
        entry.target.session = session_id.into();
        entry.location.session = session_name.into();
        entry.working_dir = cwd.map(String::from);
        entry
    })
    .collect()
}

fn grouped_app(entries: Vec<Entry>, group_by: GroupBy) -> App {
    let mut app = App {
        connected: true,
        group_by,
        ..App::default()
    };
    app.replace_snapshot(Snapshot {
        entries,
        warnings: Vec::new(),
    });
    app
}

#[test]
fn default_view_preserves_the_original_flat_list_and_navigation() {
    let mut expected = Snapshot {
        entries: grouping_entries(),
        warnings: Vec::new(),
    };
    expected.sort();
    let mut app = App {
        connected: true,
        ..App::default()
    };
    app.replace_snapshot(expected.clone());
    assert_eq!(app.group_by, GroupBy::None);
    assert_eq!(app.snapshot.entries, expected.entries);
    let mut terminal = terminal(100, 12);
    app.handle_event(key(KeyCode::Down));
    let rows = render(&mut app, &mut terminal);
    assert!(rows[0].starts_with("brachio | 6 agents"));
    assert!(!rows.iter().any(|row| row.contains("Group:")));
    assert!(rows[10].contains("g/G Group"));
    assert!(!rows[10].contains("Tab Groups"));
    assert_eq!(app.hits.len(), 6);
    assert_eq!(
        app.list_rows(),
        (0..6).map(ListRow::Agent).collect::<Vec<_>>()
    );
    let selected = app.selected.clone();
    for code in [KeyCode::Tab, KeyCode::BackTab] {
        assert!(app.handle_event(key(code)).is_none());
        assert_eq!(app.selected, selected);
        assert_eq!(app.hits.len(), 6);
    }
    let target = expected.entries[1].clone();
    assert_eq!(navigation(app.handle_event(key(KeyCode::Enter))), target);
    app.navigating = false;
    render(&mut app, &mut terminal);
    assert_eq!(navigation(click_agent(&mut app, &target.id)), target);
}

#[test]
fn headers_precede_each_group_and_have_separate_toggle_only_targets() {
    for (mode, separators) in [
        (GroupBy::None, vec![]),
        (GroupBy::Agent, vec![0, 3, 6]),
        (GroupBy::Status, vec![0, 2, 4, 6, 8]),
        (GroupBy::Cwd, vec![0, 3, 6]),
        (GroupBy::Session, vec![0, 3, 6]),
    ] {
        let mut app = grouped_app(grouping_entries(), mode);
        let layout = app.list_rows();
        let actual: Vec<_> = layout
            .iter()
            .enumerate()
            .filter_map(|(row, value)| matches!(value, ListRow::Header { .. }).then_some(row))
            .collect();
        assert_eq!(actual, separators, "{mode:?}");
        assert_eq!(layout.last(), Some(&ListRow::Agent(5)));
        let mut terminal = terminal(160, 18);
        let rows = render(&mut app, &mut terminal);
        assert_eq!(app.hits.len(), 6);
        assert_eq!(rows[2], "─".repeat(160));
        assert_eq!(rows.iter().filter(|row| row.contains('─')).count(), 1);
        assert!(app.hits.iter().all(|hit| hit.area.y > 2));
        assert!(rows[16].contains("g/G Group"));
        assert!(rows[16].contains("q Quit"));
        assert_eq!(app.group_hits.len(), separators.len());
        let selected = app.selected.clone();
        let headers: Vec<_> = app
            .group_hits
            .iter()
            .map(|hit| (hit.area, hit.key.clone()))
            .collect();
        for (area, key) in headers {
            assert!(rows[usize::from(area.y)].contains('▾'));
            assert!(!app.hits.iter().any(|hit| hit.area.y == area.y));
            assert!(app.handle_event(click(area.x, area.y)).is_none());
            assert!(app.collapsed.contains(&key));
            assert_eq!(app.selected, selected);
            assert!(!app.navigating);
            render(&mut app, &mut terminal);
            let area = app
                .group_hits
                .iter()
                .find(|hit| hit.key == key)
                .unwrap()
                .area;
            assert!(app.handle_event(click(area.x, area.y)).is_none());
            assert!(!app.collapsed.contains(&key));
            render(&mut app, &mut terminal);
        }
        assert_eq!(
            navigation(app.handle_event(key(KeyCode::Enter))).id,
            selected.unwrap()
        );
    }
    assert!(
        grouped_app(Vec::new(), GroupBy::Status)
            .list_rows()
            .is_empty()
    );
}

#[test]
fn scrolling_counts_header_rows_and_arrows_skip_expanded_headers() {
    let mut app = grouped_app(
        vec![
            entry(0, Status::Blocked),
            entry(1, Status::Blocked),
            entry(2, Status::Working),
            entry(3, Status::Working),
        ],
        GroupBy::Status,
    );
    let mut terminal = terminal(100, 9);
    render(&mut app, &mut terminal);
    let selected = app.selected.clone();
    app.handle_event(mouse(MouseEventKind::ScrollDown, 0, 3));
    let rows = render(&mut app, &mut terminal);
    assert_eq!(app.scroll, 2);
    assert!(rows[0].contains("4 agents"));
    assert_eq!(app.selected, selected);
    assert!(rows[4].contains("Working (2)"));
    assert!(!app.hits.iter().any(|hit| hit.area.y == 4));
    let area = agent_area(&app, &entry(3, Status::Working).id);
    assert_eq!(
        navigation(app.handle_event(click(area.x, area.y))),
        entry(3, Status::Working)
    );
    app.navigating = false;
    app.handle_event(key(KeyCode::Up));
    render(&mut app, &mut terminal);
    assert_eq!(app.selected, Some(entry(2, Status::Working).id));
    app.handle_event(key(KeyCode::Up));
    render(&mut app, &mut terminal);
    assert_eq!(app.selected, Some(entry(1, Status::Blocked).id));
    assert_eq!(app.selected_row(), Some(2));
}

#[test]
fn refresh_reveals_selection_when_new_headers_change_only_its_screen_row() {
    let mut app = grouped_app(
        vec![entry(0, Status::Working), entry(1, Status::Working)],
        GroupBy::Status,
    );
    let mut terminal = terminal(100, 5);
    app.handle_event(key(KeyCode::Down));
    render(&mut app, &mut terminal);
    assert_eq!(app.scroll, 1);
    let selected = app.selected.clone();
    let mut snapshot = app.snapshot.clone();
    snapshot.entries[0].status = Status::Blocked;
    app.replace_snapshot(snapshot);
    render(&mut app, &mut terminal);
    assert_eq!(app.selected_index(), Some(1));
    assert_eq!(app.selected_row(), Some(3));
    assert_eq!(app.scroll, 3);
    assert_eq!(app.selected, selected);
    assert_eq!(app.hits.len(), 1);
    assert_eq!(Some(&app.hits[0].entry.id), selected.as_ref());
}

#[test]
fn header_only_view_has_no_agent_target_and_click_only_toggles() {
    let mut app = grouped_app(
        vec![entry(0, Status::Blocked), entry(1, Status::Working)],
        GroupBy::Status,
    );
    let mut terminal = terminal(100, 5);
    render(&mut app, &mut terminal);
    app.scroll = 2;
    app.reveal_selection = false;
    let rows = render(&mut app, &mut terminal);
    assert!(rows[0].contains("2 agents"));
    assert!(rows[2].contains("Working (1)"));
    assert!(app.hits.is_empty());
    assert_eq!(app.group_hits.len(), 1);
    assert!(app.handle_event(click(0, 2)).is_none());
    assert_eq!(app.selected_index(), Some(0));
    assert!(!app.navigating);
    app.handle_event(key(KeyCode::Down));
    assert!(app.handle_event(key(KeyCode::Enter)).is_none());
    render(&mut app, &mut terminal);
    assert_eq!(app.selected_index(), Some(1));
    assert_eq!(app.hits.len(), 1);
    assert_eq!(app.hits[0].entry, entry(1, Status::Working));
}

#[test]
fn every_grouping_mode_has_deterministic_groups_and_status_priority_within_groups() {
    let entries = grouping_entries();
    for (mode, order) in [
        (GroupBy::None, [3, 2, 4, 1, 5, 0]),
        (GroupBy::Agent, [4, 1, 2, 5, 3, 0]),
        (GroupBy::Status, [3, 2, 4, 1, 5, 0]),
        (GroupBy::Cwd, [2, 4, 3, 0, 1, 5]),
        (GroupBy::Session, [2, 1, 3, 5, 4, 0]),
    ] {
        let expected: Vec<_> = order.map(|index| entries[index].clone()).into();
        for input in [entries.clone(), entries.iter().cloned().rev().collect()] {
            let mut app = grouped_app(input, mode);
            assert_eq!(app.snapshot.entries, expected, "{mode:?}");
            let rows = render(&mut app, &mut terminal(120, 18));
            assert_eq!(rows[0].contains("Group:"), mode != GroupBy::None);
            if mode != GroupBy::None {
                assert!(rows[0].contains(&format!("Group: {}", mode.label())));
            }
            assert!(!rows[0].contains("(g)"));
            assert_eq!(app.snapshot.entries, expected, "draw changed {mode:?}");
            assert_eq!(app.hits.len(), expected.len());
            for (hit, entry) in app.hits.iter().zip(&expected) {
                assert_eq!(&hit.entry, entry);
                assert_eq!(
                    rows[usize::from(hit.area.y)].contains(&entry.name),
                    mode != GroupBy::Agent
                );
            }
            let narrow = render(&mut app, &mut terminal(35, 12));
            assert_eq!(narrow[0].contains("Group:"), mode != GroupBy::None);
            if mode != GroupBy::None {
                assert!(narrow[0].contains(&format!("Group: {}", mode.label())));
            }
        }
    }
}

#[test]
fn mode_key_cycles_all_modes_without_changing_the_selected_identity() {
    let mut app = grouped_app(grouping_entries(), GroupBy::default());
    let selected = app.selected.clone();
    for mode in [
        GroupBy::Agent,
        GroupBy::Status,
        GroupBy::Cwd,
        GroupBy::Session,
        GroupBy::None,
    ] {
        assert!(app.handle_event(key(KeyCode::Char('g'))).is_none());
        assert_eq!(app.group_by, mode);
        assert_eq!(app.selected, selected);
        let rows = render(&mut app, &mut terminal(100, 8));
        assert_eq!(rows[0].contains("Group:"), mode != GroupBy::None);
        if mode != GroupBy::None {
            assert!(rows[0].contains(&format!("Group: {}", mode.label())));
        }
        assert!(!rows[0].contains("(g)"));
        assert!(rows[6].contains("g/G Group"));
        assert!(
            app.hits
                .iter()
                .any(|hit| Some(&hit.entry.id) == selected.as_ref())
        );
        assert!(!app.navigating);
    }
}

#[test]
fn tab_and_backtab_jump_to_group_starts_and_wrap_in_every_mode() {
    let entries = grouping_entries();
    for (mode, starts) in [
        (GroupBy::Agent, vec![4, 2, 3]),
        (GroupBy::Status, vec![3, 2, 4, 1, 5]),
        (GroupBy::Cwd, vec![2, 3, 1]),
        (GroupBy::Session, vec![2, 3, 4]),
    ] {
        let mut app = grouped_app(entries.clone(), mode);
        app.selected = None;
        for &index in starts.iter().chain(starts.iter().take(1)) {
            assert!(app.handle_event(key(KeyCode::Tab)).is_none());
            assert_eq!(app.selected, Some(entries[index].id.clone()), "{mode:?}");
            assert!(!app.navigating);
        }
        for &index in starts.iter().rev().chain(starts.iter().rev().take(1)) {
            let event = Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
            assert!(app.handle_event(event).is_none());
            assert_eq!(app.selected, Some(entries[index].id.clone()), "{mode:?}");
            assert!(!app.navigating);
        }
        app.selected = None;
        app.handle_event(key(KeyCode::BackTab));
        assert_eq!(
            app.selected,
            Some(entries[*starts.last().unwrap()].id.clone())
        );
    }
}

#[test]
fn group_jumps_skip_the_current_group_even_when_selection_is_inside_it() {
    for mode in [GroupBy::Agent, GroupBy::Cwd, GroupBy::Session] {
        let mut app = grouped_app(grouping_entries(), mode);
        app.select_index(1);
        let next = app.snapshot.entries[2].id.clone();
        app.handle_event(key(KeyCode::Tab));
        assert_eq!(app.selected, Some(next));
        app.select_index(3);
        let previous = app.snapshot.entries[0].id.clone();
        app.handle_event(key(KeyCode::BackTab));
        assert_eq!(app.selected, Some(previous));
    }
}

#[test]
fn empty_lists_and_single_groups_handle_group_keys_without_navigation() {
    let mut empty = app(Vec::new());
    for _ in 0..4 {
        for code in [KeyCode::Char('g'), KeyCode::Tab, KeyCode::BackTab] {
            assert!(empty.handle_event(key(code)).is_none());
            assert!(empty.selected.is_none());
            assert!(!empty.navigating);
        }
    }
    for mode in [
        GroupBy::Agent,
        GroupBy::Status,
        GroupBy::Cwd,
        GroupBy::Session,
    ] {
        let mut entries = vec![entry(0, Status::Working), entry(1, Status::Working)];
        entries[1].id.session = entries[0].id.session.clone();
        entries[1].target.session = entries[0].target.session.clone();
        entries[1].location.session = entries[0].location.session.clone();
        let mut app = grouped_app(entries, mode);
        assert!(
            app.list_rows()
                .iter()
                .all(|row| matches!(row, ListRow::Agent(_)))
        );
        for code in [KeyCode::Tab, KeyCode::BackTab] {
            app.select_index(1);
            assert!(app.handle_event(key(code)).is_none());
            assert_eq!(app.selected_index(), Some(0));
            assert!(!app.navigating);
        }
    }
}

#[test]
fn grouping_uses_canonical_agents_and_full_directories_not_display_values() {
    let mut a = entry(0, Status::Working);
    let mut b = entry(1, Status::Working);
    a.name = "same display name".into();
    b.name = a.name.clone();
    a.id.agent = "pi".into();
    b.id.agent = "codex".into();
    assert_ne!(GroupBy::Agent.compare(&a, &b), Ordering::Equal);
    a.id.agent = b.id.agent.clone();
    b.name = "different display name".into();
    assert_eq!(GroupBy::Agent.compare(&a, &b), Ordering::Equal);
    a.working_dir = Some("/first/long-prefix/project".into());
    b.working_dir = Some("/second/long-prefix/project".into());
    assert_eq!(
        clip_text(a.working_dir.as_deref().unwrap(), 10, true),
        clip_text(b.working_dir.as_deref().unwrap(), 10, true)
    );
    assert_ne!(GroupBy::Cwd.compare(&a, &b), Ordering::Equal);
}

#[test]
fn refresh_keeps_the_active_grouping_and_the_updated_exact_selection() {
    for mode in [
        GroupBy::Agent,
        GroupBy::Status,
        GroupBy::Cwd,
        GroupBy::Session,
    ] {
        let mut app = grouped_app(grouping_entries(), mode);
        app.select_index(1);
        let mut updated = app.snapshot.entries[1].clone();
        updated.status = Status::Blocked;
        updated.working_dir = Some("/work/new".into());
        updated.location.session = "Renamed session".into();
        updated.location.tab = "Moved tab".into();
        updated.target.window = "new-window".into();
        let mut snapshot = app.snapshot.clone();
        for entry in &mut snapshot.entries {
            if entry.id == updated.id {
                *entry = updated.clone();
            } else if entry.id.session == updated.id.session {
                entry.location.session = updated.location.session.clone();
            }
        }
        snapshot.entries.reverse();
        app.replace_snapshot(snapshot);
        let before_draw = app.snapshot.entries.clone();
        render(&mut app, &mut terminal(100, 8));
        assert_eq!(app.group_by, mode);
        assert_eq!(app.snapshot.entries, before_draw);
        assert!(
            app.snapshot
                .entries
                .windows(2)
                .all(|pair| { mode.compare(&pair[0], &pair[1]) != Ordering::Greater })
        );
        assert_eq!(app.selected, Some(updated.id.clone()));
        assert_eq!(navigation(app.handle_event(key(KeyCode::Enter))), updated);
    }
}

#[test]
fn mode_changes_do_not_restore_a_removed_selection_but_group_jumps_can_select() {
    for code in [KeyCode::Tab, KeyCode::BackTab] {
        let mut app = grouped_app(grouping_entries(), GroupBy::Status);
        let selected = app.selected.clone().unwrap();
        let mut snapshot = app.snapshot.clone();
        snapshot.entries.retain(|entry| entry.id != selected);
        app.replace_snapshot(snapshot);
        let message = app.message.clone();
        for _ in 0..5 {
            app.handle_event(key(KeyCode::Char('g')));
            render(&mut app, &mut terminal(100, 8));
            assert!(app.selected.is_none());
            assert!(app.selection_missing);
            assert_eq!(app.message, message);
        }
        assert!(app.handle_event(key(KeyCode::Enter)).is_none());
        assert!(app.handle_event(key(code)).is_none());
        let expected = if code == KeyCode::Tab {
            0
        } else {
            app.current_groups().last().unwrap().entries[0]
        };
        assert_eq!(app.selected_index(), Some(expected));
        assert!(!app.selection_missing);
        assert!(!app.navigating);
    }
}

#[test]
fn grouping_keys_invalidate_old_clicks_and_keep_keyboard_and_mouse_targets_equal() {
    for code in [KeyCode::Char('g'), KeyCode::Tab, KeyCode::BackTab] {
        let mut app = grouped_app(grouping_entries(), GroupBy::Status);
        let mut terminal = terminal(100, 8);
        render(&mut app, &mut terminal);
        assert!(app.handle_event(key(code)).is_none());
        let expected = app.snapshot.entries[app.selected_index().unwrap()].clone();
        assert!(app.hits.is_empty());
        assert!(app.handle_event(click(0, 3)).is_none());
        render(&mut app, &mut terminal);
        let area = app
            .hits
            .iter()
            .find(|hit| hit.entry.id == expected.id)
            .unwrap()
            .area;
        assert_eq!(
            navigation(app.handle_event(click(area.x, area.y))),
            expected
        );
        app.navigating = false;
        assert_eq!(navigation(app.handle_event(key(KeyCode::Enter))), expected);
    }
}

#[test]
fn grouping_keys_ignore_releases_and_do_not_change_details() {
    let mut app = grouped_app(grouping_entries(), GroupBy::Status);
    let selected = app.selected.clone();
    let mut terminal = terminal(100, 8);
    render(&mut app, &mut terminal);
    let hits = app.hits.len();
    for code in [KeyCode::Char('g'), KeyCode::Tab, KeyCode::BackTab] {
        let event = Event::Key(KeyEvent::new_with_kind(
            code,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        assert!(app.handle_event(event).is_none());
        assert_eq!(app.hits.len(), hits);
        assert_eq!(app.group_by, GroupBy::Status);
        assert_eq!(app.selected, selected);
    }
    app.handle_event(key(KeyCode::Char('w')));
    render(&mut app, &mut terminal);
    app.handle_event(key(KeyCode::Down));
    let scroll = app.details_scroll;
    let message = app.message.clone();
    for code in [KeyCode::Char('g'), KeyCode::Tab, KeyCode::BackTab] {
        assert!(app.handle_event(key(code)).is_none());
        assert!(app.details);
        assert_eq!(app.group_by, GroupBy::Status);
        assert_eq!(app.selected, selected);
        assert_eq!(app.details_scroll, scroll);
        assert_eq!(app.message, message);
        assert!(!app.navigating);
    }
}

#[test]
fn group_jumps_reveal_the_selected_group_after_manual_scroll_and_resize() {
    let entries = (0..20)
        .map(|index| {
            let mut entry = entry(index, Status::Working);
            entry.working_dir = Some(format!("/work/group-{:02}", index / 5));
            entry
        })
        .collect();
    let mut app = grouped_app(entries, GroupBy::Cwd);
    let mut terminal = terminal(80, 8);
    render(&mut app, &mut terminal);
    app.handle_event(mouse(MouseEventKind::ScrollDown, 0, 3));
    render(&mut app, &mut terminal);
    assert_eq!(app.scroll, 3);
    for index in [5, 10, 15, 0] {
        assert!(app.handle_event(key(KeyCode::Tab)).is_none());
        render(&mut app, &mut terminal);
        assert_eq!(app.selected_index(), Some(index));
        assert!(
            app.hits
                .iter()
                .any(|hit| Some(&hit.entry.id) == app.selected.as_ref())
        );
    }
    app.handle_event(Event::Resize(40, 5));
    terminal.backend_mut().resize(40, 5);
    terminal.resize(Rect::new(0, 0, 40, 5)).unwrap();
    app.handle_event(key(KeyCode::BackTab));
    render(&mut app, &mut terminal);
    assert_eq!(app.selected_index(), Some(15));
    assert_eq!(app.hits.len(), 1);
    assert_eq!(app.hits[0].entry.id, app.selected.clone().unwrap());
    assert!(!app.navigating);
}

#[test]
fn rows_are_grouped_by_model_priority_with_name_status_and_location() {
    let statuses = [
        Status::Idle,
        Status::Working,
        Status::Done,
        Status::Error,
        Status::Blocked,
    ];
    let mut app = app(statuses
        .into_iter()
        .enumerate()
        .map(|(i, status)| entry(i, status))
        .collect());
    let rows = render(&mut app, &mut terminal(100, 16));
    for (i, entry) in app.snapshot.entries.iter().enumerate() {
        let row = &rows[usize::from(app.hits[i].area.y)];
        assert!(row.contains(&entry.name));
        assert!(row.contains(entry.status.label()));
        for label in [
            &entry.location.session,
            &entry.location.tab,
            &entry.location.pane,
        ] {
            assert!(row.contains(label.as_ref()));
        }
    }
    assert_eq!(app.snapshot.entries[0].status, Status::Blocked);
    assert_eq!(app.snapshot.entries[4].status, Status::Idle);
    assert_eq!(app.selected, Some(app.snapshot.entries[0].id.clone()));
}

#[test]
fn equal_statuses_use_the_same_location_and_identity_order_as_the_model() {
    let mut a = entry(1, Status::Working);
    let mut b = entry(2, Status::Working);
    a.location = Location {
        session: "same".into(),
        tab: "same".into(),
        pane: "same".into(),
    };
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
    assert!(rows[3].contains("Blocked"));
    for label in ["session-00", "tab-00", "pane-00"] {
        assert!(rows[3].contains(label));
    }
    assert!(!rows[4].contains('界'));
    assert!(rows[7].contains("Notice (w)"));
    assert!(rows[7].contains("1 warnings (w)"));
    assert!(!rows[7].contains("failure"));
    for cell in &terminal.backend().buffer().content {
        assert_eq!(plain(cell.symbol()), cell.symbol());
    }
}

#[test]
fn independent_location_columns_keep_names_instead_of_opaque_id_suffixes() {
    let mut agent = entry(0, Status::Working);
    agent.location = Location {
        session: "brachio/worktree".into(),
        tab: "Review changes".into(),
        pane: "2: codex".into(),
    };
    agent.working_dir = Some("/Users/test/projects/brachio".into());
    agent.target = Target {
        session: "session:00000000-0000-0000-0000-000000000001".into(),
        window: "window:00000000-0000-0000-0000-000000000002".into(),
        block: "block:00000000-0000-0000-0000-000000000003".into(),
    };
    let mut app = app(vec![agent]);
    for (width, height) in VIEW_SIZES {
        let mut terminal = terminal(width, height);
        let rows = render(&mut app, &mut terminal);
        assert_independent_fields(&app, &terminal);
        let layout = app_columns(&app, app.hits[0].area);
        for (index, value) in [
            (4, "brachio/worktree"),
            (5, "Review changes"),
            (6, "2: codex"),
        ] {
            let area = layout.areas[index];
            if area.width > 0 {
                assert_cell_text(terminal.backend().buffer(), area, value, false);
            }
        }
        assert!(!rows[3].contains("00000"));
        assert!(!rows[1].contains("Location"));
    }
}

#[test]
fn directory_clipping_preserves_the_project_tail_and_graphemes() {
    let clipped = clip_text("/home/person/界e\u{301}/brachio", 13, true);
    assert!(clipped.starts_with('…'));
    assert!(clipped.ends_with("/brachio"));
    assert!(Line::raw(clipped.as_str()).width() <= 13);
    assert!(!clipped.starts_with("…\u{301}"));
    assert_eq!(
        clip_text("/work/\x1b\n\u{202e}repo", 80, true),
        "/work/repo"
    );
    assert_eq!(clip_text("/work/repo", 1, true), "…");
    assert_eq!(clip_text("/work/repo", 0, true), "");
    let mut app = many(1);
    let rows = render(&mut app, &mut terminal(80, 8));
    assert!(rows[1].contains("Working directory"));
    assert!(rows[3].contains("Unknown"));
}

#[test]
fn details_include_complete_selected_location_directory_and_exact_ids() {
    let mut selected = entry(0, Status::Working);
    selected.location = Location {
        session: "team/project".into(),
        tab: "Review / tests".into(),
        pane: "2: coding agent".into(),
    };
    selected.working_dir = Some(format!("/work/{}/brachio", "long-directory/".repeat(30)));
    let expected = selected.clone();
    let mut app = app(vec![selected]);
    app.handle_event(key(KeyCode::Char('w')));
    let mut terminal = terminal(80, 8);
    let rows = render(&mut app, &mut terminal);
    let mut text: String = rows[1..7].iter().map(|row| row.trim_end()).collect();
    for _ in 0..app.details_limit {
        app.handle_event(key(KeyCode::Down));
        let rows = render(&mut app, &mut terminal);
        text.push_str(rows[6].trim_end());
    }
    for value in [
        expected.location.session.as_ref(),
        expected.location.tab.as_ref(),
        expected.location.pane.as_ref(),
        expected.working_dir.as_deref().unwrap(),
        expected.target.session.as_str(),
        expected.target.window.as_str(),
        expected.target.block.as_str(),
    ] {
        assert!(text.contains(value), "missing {value:?}");
    }
    assert!(text.contains("Working directory:"));
    assert!(text.contains("Session ID:"));
    assert!(text.contains("Tab ID:"));
    assert!(text.contains("Pane ID:"));
    app.handle_event(key(KeyCode::Esc));
    assert_eq!(navigation(app.handle_event(key(KeyCode::Enter))), expected);
}

#[test]
fn all_tiny_dimensions_render_without_overflow_or_hidden_hit_regions() {
    for width in [0, 1, 2, 8, 17, 80] {
        for height in [0, 1, 2, 3, 4, 5, 24] {
            for mode in [
                GroupBy::None,
                GroupBy::Agent,
                GroupBy::Status,
                GroupBy::Cwd,
                GroupBy::Session,
            ] {
                let mut app = grouped_app(grouping_entries(), mode);
                let mut terminal = terminal(width, height);
                render(&mut app, &mut terminal);
                app.handle_event(key(KeyCode::Tab));
                render(&mut app, &mut terminal);
                for area in app
                    .hits
                    .iter()
                    .map(|hit| hit.area)
                    .chain(app.group_hits.iter().map(|hit| hit.area))
                {
                    assert!(area.width > 0);
                    assert_eq!(area.height, 1);
                    assert!(area.right() <= width);
                    assert!(area.bottom() <= height.saturating_sub(2));
                }
                if width == 0 || height <= 2 {
                    assert_no_hits(&app);
                }
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
    assert_eq!(app.scroll, 10);
    assert_eq!(app.hits.len(), 3);
    assert_eq!(app.hits[2].entry, entry(12, Status::Working));
    assert!(rows[5].starts_with('>'));
    app.handle_event(key(KeyCode::Up));
    render(&mut app, &mut terminal);
    assert_eq!(app.scroll, 10);
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
    selected.location = Location {
        session: "new session".into(),
        tab: "new tab".into(),
        pane: "pane-1".into(),
    };
    selected.working_dir = Some("/work/new-directory".into());
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
    assert!(!rows[3].starts_with('>'));
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
    wheel(&mut app, true);
    assert!(app.selected.is_none());
    render(&mut app, &mut terminal);
    assert_eq!(
        navigation(click_agent(&mut app, &entry(1, Status::Working).id)),
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
    let actual = navigation(click_agent(&mut pointer, &expected.id));
    assert_eq!(actual, expected);
    assert_eq!(pointer.selected, Some(actual.id));
}

#[test]
fn click_requires_a_draw_and_only_drawn_entry_rows_are_targets() {
    let mut app = many(1);
    assert!(app.handle_event(click(0, 3)).is_none());
    let mut terminal = terminal(80, 8);
    for (x, y) in [
        (0, 0),
        (0, 1),
        (0, 2),
        (0, 4),
        (0, 6),
        (0, 7),
        (80, 3),
        (0, 8),
    ] {
        render(&mut app, &mut terminal);
        assert!(app.handle_event(click(x, y)).is_none());
        assert!(!app.navigating);
    }
    render(&mut app, &mut terminal);
    let area = agent_area(&app, &entry(0, Status::Working).id);
    assert!(
        app.handle_event(mouse(
            MouseEventKind::Down(MouseButton::Right),
            area.x,
            area.y
        ))
        .is_none()
    );
    render(&mut app, &mut terminal);
    assert_eq!(
        navigation(click_agent(&mut app, &entry(0, Status::Working).id)),
        entry(0, Status::Working)
    );
}

#[test]
fn snapshot_replacement_invalidates_even_an_unchanged_hit_map() {
    let mut app = many(2);
    let mut terminal = terminal(80, 8);
    render(&mut app, &mut terminal);
    app.replace_snapshot(app.snapshot.clone());
    assert!(app.handle_event(click(0, 3)).is_none());
    render(&mut app, &mut terminal);
    app.replace_snapshot(Snapshot {
        entries: vec![entry(1, Status::Blocked), entry(0, Status::Working)],
        warnings: Vec::new(),
    });
    assert!(app.handle_event(click(0, 3)).is_none());
    render(&mut app, &mut terminal);
    assert_eq!(
        navigation(click_agent(&mut app, &entry(1, Status::Blocked).id)),
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
    assert!(app.handle_event(click(0, 3)).is_none());
    assert!(app.selected.is_none());
    assert!(app.message.contains("no longer available"));
    render(&mut app, &mut terminal);
    assert_eq!(
        navigation(click_agent(&mut app, &entry(1, Status::Working).id)),
        entry(1, Status::Working)
    );
}

#[test]
fn non_mutating_events_preserve_a_following_click_in_the_same_input_batch() {
    let mut app = many(2);
    render(&mut app, &mut terminal(80, 8));
    for event in [
        mouse(MouseEventKind::Moved, 1, 4),
        mouse(MouseEventKind::Up(MouseButton::Left), 1, 4),
        mouse(MouseEventKind::Drag(MouseButton::Left), 1, 4),
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
        navigation(click_agent(&mut app, &entry(1, Status::Working).id)),
        entry(1, Status::Working)
    );
}

#[test]
fn keyboard_input_invalidates_click_regions_until_the_next_draw() {
    let mut app = many(2);
    let mut terminal = terminal(80, 8);
    render(&mut app, &mut terminal);
    app.handle_event(key(KeyCode::Down));
    assert!(app.handle_event(click(0, 3)).is_none());
    assert_eq!(app.selected, Some(entry(1, Status::Working).id));
    render(&mut app, &mut terminal);
    assert_eq!(
        navigation(click_agent(&mut app, &entry(0, Status::Working).id)),
        entry(0, Status::Working)
    );
}

#[test]
fn wheel_scrolls_the_view_and_click_uses_the_drawn_offset() {
    let mut app = many(15);
    let mut terminal = terminal(80, 8);
    render(&mut app, &mut terminal);
    app.handle_event(mouse(MouseEventKind::ScrollDown, 0, 3));
    assert_eq!(app.scroll, 3);
    assert_eq!(app.selected, Some(entry(0, Status::Working).id));
    assert!(app.handle_event(click(0, 3)).is_none());
    render(&mut app, &mut terminal);
    assert_eq!(app.hits[0].entry, entry(3, Status::Working));
    assert_eq!(
        navigation(click_agent(&mut app, &entry(3, Status::Working).id)),
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
        app.handle_event(mouse(MouseEventKind::ScrollDown, 0, 3));
    }
    assert_eq!(app.scroll, 12);
    render(&mut app, &mut terminal);
    assert_eq!(app.hits.last().unwrap().entry, entry(14, Status::Working));
    for _ in 0..20 {
        app.handle_event(mouse(MouseEventKind::ScrollUp, 0, 3));
    }
    assert_eq!(app.scroll, 0);
}

#[test]
fn unchanged_refresh_preserves_manual_scroll_and_missing_selection_notice() {
    let mut app = many(15);
    let mut terminal = terminal(80, 8);
    render(&mut app, &mut terminal);
    app.handle_event(mouse(MouseEventKind::ScrollDown, 0, 3));
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
    app.handle_event(mouse(MouseEventKind::ScrollDown, 0, 3));
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
        navigation(click_agent(&mut app, &entry(12, Status::Working).id)),
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
    assert!(click_agent(&mut app, &entry(1, Status::Working).id).is_none());
    assert!(!app.navigating);
    app.connected = true;
    let target = navigation(app.handle_event(key(KeyCode::Enter)));
    assert_eq!(target, entry(1, Status::Working));
    assert!(app.navigating);
    assert!(app.handle_event(key(KeyCode::Enter)).is_none());
    assert!(app.message.contains("already in progress"));
    let rows = render(&mut app, &mut terminal);
    assert!(rows[7].contains("Navigating"));
    assert!(click_agent(&mut app, &entry(0, Status::Working).id).is_none());
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
    assert!(rows[7].contains("Notice (w)"));
    assert!(rows[7].contains("2 warnings (w)"));
    assert!(!rows[7].contains("Remote Control denied"));
    app.connected = false;
    let rows = render(&mut app, &mut terminal);
    assert!(rows[7].contains("Disconnected; navigation disabled"));
    assert!(rows[7].contains("Notice (w)"));
    assert!(rows[3].contains("agent-00"));
}

#[test]
fn footer_keeps_diagnostics_in_details_instead_of_full_messages_or_ids() {
    let mut app = many(1);
    let client = "client:00000000-0000-0000-0000-000000000001";
    let message = format!("Target app {client}; session focus is shared with other clients");
    app.message = message.clone();
    app.client = Some(client.into());
    app.snapshot.warnings = vec!["First warning".into(), "Second warning".into()];
    let mut terminal = terminal(80, 8);
    for connected in [true, false] {
        app.connected = connected;
        let rows = render(&mut app, &mut terminal);
        assert!(rows[6].contains("g/G Group"));
        assert!(rows[7].contains("2 warnings (w)"));
        assert!(!rows[7].contains("Target app"));
        assert!(!rows[7].contains(client));
        assert_eq!(app.message, message);
        app.handle_event(key(KeyCode::Char('w')));
        let text = overlay_text(&mut app, &mut terminal);
        assert!(text.contains(&message));
        assert!(text.contains(&format!("Client ID: {client}")));
        assert!(text.contains("Session focus is shared with other connected clients."));
        for warning in &app.snapshot.warnings {
            assert!(text.contains(warning));
        }
        app.handle_event(key(KeyCode::Esc));
    }
    app.message = "Navigation failed: Remote Control denied".into();
    let rows = render(&mut app, &mut terminal);
    assert!(rows[7].contains("Disconnected; navigation disabled"));
    assert!(!rows[7].contains("Remote Control denied"));
    app.handle_event(key(KeyCode::Char('w')));
    assert!(overlay_text(&mut app, &mut terminal).contains(&app.message));
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
    assert!(rows[7].contains("Esc/q Back"));
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
    let compact = |value: &str| {
        value
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
    };
    assert!(compact(&text).contains(&compact(&display_text(&message))));
    for warning in &warnings {
        assert!(compact(&text).contains(&compact(&display_text(warning))));
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
        app.handle_event(mouse(MouseEventKind::ScrollDown, 0, 3));
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
    assert!(app.handle_event(click(0, 3)).is_none());
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
    render(&mut app, &mut terminal);
    assert!(app.details_scroll <= app.details_limit);
    // Selected location metadata remains available even with no warnings.
    for _ in 0..app.details_limit {
        app.handle_event(key(KeyCode::Up));
    }
    let rows = render(&mut app, &mut terminal);
    assert!(rows[1].contains("Message: No current message."));
    assert_eq!(app.details_scroll, 0);
    let text = overlay_text(&mut app, &mut terminal);
    assert!(text.contains("Warnings: 0"));
    assert!(text.contains("No snapshot warnings."));
    assert!(app.details_limit > 0);
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
    assert!(app.handle_event(key(KeyCode::Char('q'))).is_none());
    assert!(!app.details);
    for event in [
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
    assert!(app.handle_event(click(0, 3)).is_none());
    assert!(app.message.contains("list changed"));
    assert!(!app.navigating);
    app.snapshot.entries.remove(0);
    assert!(app.handle_event(key(KeyCode::Enter)).is_none());
    assert!(app.selected.is_none());
    assert!(app.message.contains("no longer available"));
}

fn apply_filter(app: &mut App, query: &str) {
    assert!(app.handle_event(key(KeyCode::Char('/'))).is_none());
    assert!(app.filter_edit.is_some());
    for _ in 0..app.query.chars().count() {
        app.handle_event(key(KeyCode::Backspace));
    }
    assert!(app.handle_event(Event::Paste(query.into())).is_none());
    assert!(app.handle_event(key(KeyCode::Enter)).is_none());
    assert!(app.filter_edit.is_none());
    assert_eq!(app.query, query);
}

#[test]
fn folds_keep_the_exact_member_identity_and_highlight_the_collapsed_header() {
    for mode in [GroupBy::Agent, GroupBy::Cwd, GroupBy::Session] {
        let mut app = grouped_app(grouping_entries(), mode);
        app.select_index(1);
        let expected = app.snapshot.entries[1].clone();
        let group = app.selected_group().unwrap();
        let mut terminal = terminal(120, 18);
        render(&mut app, &mut terminal);
        assert!(app.handle_event(key(KeyCode::Left)).is_none());
        assert_no_hits(&app);
        assert_eq!(app.selected, Some(expected.id.clone()));
        let rows = render(&mut app, &mut terminal);
        let header = app
            .group_hits
            .iter()
            .find(|hit| hit.key == group)
            .unwrap()
            .area;
        assert_eq!(
            app.selected_row(),
            Some(usize::from(header.y - app.viewport.y) + app.scroll)
        );
        assert!(rows[usize::from(header.y)].contains('▸'));
        assert_eq!(
            terminal.backend().buffer()[(header.x, header.y)].bg,
            Color::DarkGray
        );
        assert!(!app.hits.iter().any(|hit| hit.entry.id == expected.id));
        assert!(app.handle_event(key(KeyCode::Enter)).is_none());
        assert!(!app.collapsed.contains(&group));
        assert!(!app.navigating);
        assert_eq!(app.selected, Some(expected.id.clone()));
        assert_eq!(navigation(app.handle_event(key(KeyCode::Enter))), expected);
        app.navigating = false;
        app.handle_event(key(KeyCode::Left));
        app.handle_event(key(KeyCode::Right));
        assert!(!app.collapsed.contains(&group));
        assert_eq!(app.selected, Some(expected.id));
    }
}

#[test]
fn bounded_arrows_keep_the_non_first_member_of_a_collapsed_boundary_group() {
    for (index, arrow) in [(1, KeyCode::Up), (5, KeyCode::Down)] {
        let mut app = grouped_app(grouping_entries(), GroupBy::Agent);
        app.select_index(index);
        let expected = app.snapshot.entries[index].clone();
        app.handle_event(key(KeyCode::Left));
        assert!(app.handle_event(key(arrow)).is_none());
        assert_eq!(app.selected, Some(expected.id.clone()));
        assert!(app.handle_event(key(KeyCode::Enter)).is_none());
        assert!(!app.navigating);
        assert_eq!(app.selected, Some(expected.id.clone()));
        assert_eq!(navigation(app.handle_event(key(KeyCode::Enter))), expected);
    }
}

#[test]
fn arrows_visit_collapsed_headers_and_skip_expanded_headers_without_wrapping() {
    let mut app = grouped_app(grouping_entries(), GroupBy::Agent);
    let data = app.snapshot.entries.clone();
    app.select_index(0);
    app.handle_event(key(KeyCode::Left));
    app.select_index(2);
    app.handle_event(key(KeyCode::Left));
    app.select_index(4);
    app.handle_event(key(KeyCode::Left));
    assert_eq!(app.list_rows().len(), 3);
    app.selected = None;
    for index in [0, 2, 4, 4] {
        app.handle_event(key(KeyCode::Down));
        assert_eq!(app.selected, Some(data[index].id.clone()));
        assert!(matches!(
            app.list_rows()[app.selected_row().unwrap()],
            ListRow::Header {
                collapsed: true,
                ..
            }
        ));
    }
    for index in [2, 0, 0] {
        app.handle_event(key(KeyCode::Up));
        assert_eq!(app.selected, Some(data[index].id.clone()));
    }
    app.handle_event(key(KeyCode::Right));
    for (code, index) in [
        (KeyCode::Down, 1),
        (KeyCode::Down, 2),
        (KeyCode::Up, 1),
        (KeyCode::Up, 0),
    ] {
        app.handle_event(key(code));
        assert_eq!(app.selected_index(), Some(index));
    }
}

#[test]
fn enter_on_a_collapsed_header_expands_even_when_navigation_is_blocked() {
    for (connected, navigating) in [(false, false), (true, true)] {
        let mut app = grouped_app(grouping_entries(), GroupBy::Agent);
        app.connected = connected;
        app.navigating = navigating;
        let selected = app.selected.clone();
        app.handle_event(key(KeyCode::Left));
        let group = app.selected_group().unwrap();
        assert!(app.collapsed.contains(&group));
        assert!(app.handle_event(key(KeyCode::Enter)).is_none());
        assert!(!app.collapsed.contains(&group));
        assert_eq!(app.selected, selected);
        assert_eq!(app.navigating, navigating);
        assert!(app.handle_event(key(KeyCode::Enter)).is_none());
        assert!(app.message.contains(if connected {
            "already in progress"
        } else {
            "disconnected"
        }));
    }
}

#[test]
fn folds_persist_across_group_cycles_filter_changes_and_refresh_then_prune_removed_groups() {
    let mut app = grouped_app(grouping_entries(), GroupBy::Agent);
    app.select_index(1);
    let selected = app.selected.clone();
    let group = app.selected_group().unwrap();
    app.handle_event(key(KeyCode::Left));
    for _ in 0..5 {
        app.handle_event(key(KeyCode::Char('g')));
    }
    assert_eq!(app.group_by, GroupBy::Agent);
    assert!(app.collapsed.contains(&group));
    apply_filter(&mut app, "agent:codex");
    assert_eq!(app.current_groups().len(), 1);
    assert!(
        app.list_rows()
            .iter()
            .all(|row| matches!(row, ListRow::Agent(_)))
    );
    app.handle_event(key(KeyCode::Esc));
    assert!(app.collapsed.contains(&group));
    let mut refreshed = app.snapshot.clone();
    refreshed.entries.reverse();
    app.replace_snapshot(refreshed);
    assert!(app.collapsed.contains(&group));
    assert_eq!(app.selected, selected);
    let mut removed = app.snapshot.clone();
    removed
        .entries
        .retain(|entry| GroupBy::Agent.key(entry).as_ref() != Some(&group));
    app.replace_snapshot(removed);
    assert!(!app.collapsed.contains(&group));
    assert!(app.selected.is_none());
    assert!(app.selection_missing);
    assert!(app.handle_event(key(KeyCode::Enter)).is_none());
}

#[test]
fn headers_and_counts_include_only_matching_members_and_disappear_for_one_group() {
    for mode in [
        GroupBy::Agent,
        GroupBy::Status,
        GroupBy::Cwd,
        GroupBy::Session,
    ] {
        let mut app = grouped_app(grouping_entries(), mode);
        apply_filter(&mut app, "cwd:/work");
        let rows = app.list_rows();
        let headers: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                ListRow::Header {
                    key,
                    first,
                    count,
                    collapsed,
                    ..
                } => Some((key, *first, *count, *collapsed)),
                _ => None,
            })
            .collect();
        assert!(headers.len() >= 2);
        assert_eq!(
            headers.iter().map(|(_, _, count, _)| count).sum::<usize>(),
            4
        );
        for (key, first, count, collapsed) in headers {
            let matching: Vec<_> = matching_indices(&app.snapshot, &app.query)
                .into_iter()
                .filter(|&index| mode.key(&app.snapshot.entries[index]).as_ref() == Some(key))
                .collect();
            assert_eq!(first, matching[0]);
            assert_eq!(count, matching.len());
            assert!(!collapsed);
        }
        let displayed = render(&mut app, &mut terminal(120, 18));
        assert!(displayed[0].contains("4/6 agents"));
        assert_eq!(app.hits.len(), 4);
        apply_filter(&mut app, "agent:codex status:error");
        assert_eq!(app.current_groups().len(), 1);
        assert_eq!(app.list_rows().len(), 1);
        assert!(matches!(app.list_rows()[0], ListRow::Agent(_)));
        app.handle_event(key(KeyCode::Down));
        app.handle_event(key(KeyCode::Left));
        render(&mut app, &mut terminal(120, 10));
        assert!(app.group_hits.is_empty());
        assert_eq!(app.hits.len(), 1);
        assert_eq!(
            navigation(app.handle_event(key(KeyCode::Enter))).status,
            Status::Error
        );
    }
}

#[test]
fn hidden_selection_is_retained_blocked_and_restored_without_implicit_reselection() {
    let mut app = many(3);
    app.handle_event(key(KeyCode::Down));
    let expected = entry(1, Status::Working);
    let mut terminal = terminal(100, 10);
    apply_filter(&mut app, "agent:agent-02");
    render(&mut app, &mut terminal);
    assert_eq!(app.selected, Some(expected.id.clone()));
    assert_eq!(app.selected_row(), None);
    assert_eq!(app.hits.len(), 1);
    assert!(app.handle_event(key(KeyCode::Enter)).is_none());
    assert!(app.message.contains("hidden by the filter"));
    assert!(!app.navigating);
    app.replace_snapshot(app.snapshot.clone());
    for _ in 0..5 {
        app.handle_event(key(KeyCode::Char('g')));
    }
    assert_eq!(app.selected, Some(expected.id.clone()));
    apply_filter(&mut app, "no-such-agent");
    let rows = render(&mut app, &mut terminal);
    assert!(rows[usize::from(app.viewport.y)].contains("No agents match the filter"));
    assert_no_hits(&app);
    for code in [
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Enter,
    ] {
        assert!(app.handle_event(key(code)).is_none());
        assert_eq!(app.selected, Some(expected.id.clone()));
    }
    assert!(app.handle_event(key(KeyCode::Esc)).is_none());
    render(&mut app, &mut terminal);
    assert!(app.selected_row().is_some());
    assert_eq!(navigation(app.handle_event(key(KeyCode::Enter))), expected);
}

#[test]
fn filter_editing_is_incremental_and_treats_q_g_and_unicode_as_text() {
    let mut first = entry(0, Status::Working);
    first.name = "qgÉ界e\u{301}👩🏽".into();
    let mut second = entry(1, Status::Working);
    second.name = "qOther".into();
    let mut app = app(vec![first.clone(), second]);
    let mut terminal = terminal(100, 10);
    render(&mut app, &mut terminal);
    app.handle_event(key(KeyCode::Char('/')));
    assert_eq!(app.filter_edit.as_deref(), Some(""));
    assert_no_hits(&app);
    for (character, count) in [('q', 2), ('g', 1), ('É', 1), ('界', 1)] {
        assert!(app.handle_event(key(KeyCode::Char(character))).is_none());
        assert_eq!(matching_indices(&app.snapshot, &app.query).len(), count);
        assert_eq!(app.group_by, GroupBy::None);
        assert!(!app.navigating);
    }
    app.handle_event(Event::Paste("e\u{301}👩🏽".into()));
    assert_eq!(app.query, first.name);
    for expected in ["qgÉ界e\u{301}", "qgÉ界", "qgÉ", "qg", "q", "", ""] {
        assert!(app.handle_event(key(KeyCode::Backspace)).is_none());
        assert_eq!(app.query, expected);
    }
    app.handle_event(Event::Paste("agent:qg".into()));
    let rows = render(&mut app, &mut terminal);
    assert!(rows[8].contains("/ agent:qg"));
    assert!(rows[9].contains("Enter Apply"));
    assert!(rows[9].contains("Esc Cancel"));
    assert!(rows[9].contains("1/2 agents"));
    assert!(app.handle_event(key(KeyCode::Enter)).is_none());
    assert_no_hits(&app);
    assert!(app.filter_edit.is_none());
    assert!(!app.navigating);
    assert_eq!(navigation(app.handle_event(key(KeyCode::Enter))), first);
}

#[test]
fn filter_cancel_restores_the_previous_query_and_escape_clears_before_quitting() {
    let mut app = many(3);
    apply_filter(&mut app, "agent:agent-00");
    let selected = app.selected.clone();
    app.handle_event(key(KeyCode::Char('/')));
    assert_eq!(app.filter_edit.as_deref(), Some("agent:agent-00"));
    app.handle_event(Event::Paste(" mismatch".into()));
    assert!(matching_indices(&app.snapshot, &app.query).is_empty());
    assert!(app.handle_event(key(KeyCode::Esc)).is_none());
    assert_eq!(app.query, "agent:agent-00");
    assert!(app.filter_edit.is_none());
    assert_eq!(app.selected, selected);
    let mut terminal = terminal(100, 10);
    render(&mut app, &mut terminal);
    assert!(app.handle_event(key(KeyCode::Esc)).is_none());
    assert!(app.query.is_empty());
    assert_no_hits(&app);
    assert!(matches!(
        app.handle_event(key(KeyCode::Esc)),
        Some(Action::Quit)
    ));
    app.handle_event(key(KeyCode::Char('/')));
    assert!(app.handle_event(key(KeyCode::Esc)).is_none());
    assert!(app.filter_edit.is_none());
}

#[test]
fn filter_fields_use_case_insensitive_and_matching_and_unknown_prefixes_stay_literal() {
    let mut first = entry(0, Status::Blocked);
    first.name = "Assistant Écho".into();
    first.working_dir = Some("/Work/Project".into());
    first.location.session = "Production".into();
    first.location.tab = "branch:Feature".into();
    first.location.pane = "Review Pane".into();
    let mut app = app(vec![first.clone(), entry(1, Status::Idle)]);
    for query in [
        "agent:ÉCHO cwd:PROJECT session:PRODUCTION status:BLOCKED review branch:FEATURE",
        "assistant session-0",
    ] {
        apply_filter(&mut app, query);
        let indices = matching_indices(&app.snapshot, &app.query);
        assert_eq!(indices.len(), 1);
        assert_eq!(app.snapshot.entries[indices[0]], first);
    }
    for query in [
        "agent:production",
        "cwd:review",
        "session:project",
        "status:assistant",
        "other:assistant",
        "agent:as.*",
        "assistant unknown",
    ] {
        apply_filter(&mut app, query);
        assert!(app.list_rows().is_empty(), "{query}");
        assert!(app.handle_event(key(KeyCode::Enter)).is_none());
        assert!(!app.navigating);
    }
}

#[test]
fn filter_paste_preserves_whitespace_separated_field_terms() {
    for separator in [" ", "\n", "\r\n", "\t", "\u{85}", "\u{2028}", "\u{a0}"] {
        let expected = entry(0, Status::Working);
        let mut other_agent = entry(2, Status::Working);
        other_agent.id.agent = "codex".into();
        let mut app = app(vec![expected.clone(), entry(1, Status::Idle), other_agent]);
        let matches = matching_indices(&app.snapshot, "agent:pi status:working");
        assert_eq!(matches.len(), 1);
        app.handle_event(key(KeyCode::Char('/')));
        assert!(
            app.handle_event(Event::Paste(format!("agent:pi{separator}status:working")))
                .is_none()
        );
        assert_eq!(
            matching_indices(&app.snapshot, &app.query),
            matches,
            "separator {separator:?}: {:?}",
            app.query
        );
        assert!(app.filter_edit.is_some());
        assert!(!app.navigating);
        assert!(app.handle_event(key(KeyCode::Enter)).is_none());
        assert_eq!(navigation(app.handle_event(key(KeyCode::Enter))), expected);
    }
}

#[test]
fn filter_paste_sanitizes_controls_limits_length_and_ignores_modified_or_release_keys() {
    let mut app = many(1);
    app.handle_event(key(KeyCode::Char('/')));
    app.handle_event(Event::Paste("q\x1b\n\u{202e}g\r\u{2066}界".into()));
    assert_eq!(app.query, "q g 界");
    for event in [
        Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)),
        Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT)),
        key(KeyCode::Char('\n')),
        Event::Key(KeyEvent::new_with_kind(
            KeyCode::Backspace,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        )),
    ] {
        assert!(app.handle_event(event).is_none());
        assert_eq!(app.query, "q g 界");
    }
    app.handle_event(Event::Paste("界".repeat(300)));
    assert_eq!(app.query.chars().count(), 256);
    app.handle_event(key(KeyCode::Char('x')));
    assert_eq!(app.query.chars().count(), 256);
    app.handle_event(key(KeyCode::Backspace));
    assert_eq!(app.query.chars().count(), 255);
    app.handle_event(key(KeyCode::Esc));
    assert!(app.query.is_empty());
    assert!(app.handle_event(Event::Paste("ignored".into())).is_none());
    assert!(app.query.is_empty());
}

#[test]
fn filter_edit_arrows_deliberately_select_visible_results_without_applying() {
    let mut app = many(4);
    app.handle_event(key(KeyCode::Char('/')));
    app.handle_event(Event::Paste("status:working".into()));
    app.handle_event(key(KeyCode::Down));
    assert_eq!(app.selected_index(), Some(1));
    app.handle_event(key(KeyCode::Up));
    assert_eq!(app.selected_index(), Some(0));
    assert!(app.filter_edit.is_some());
    assert!(!app.navigating);
    app.handle_event(key(KeyCode::Enter));
    assert_eq!(
        navigation(app.handle_event(key(KeyCode::Enter))),
        entry(0, Status::Working)
    );
}

#[test]
fn reverse_group_cycle_keeps_identity_and_invalidates_agent_and_header_hits() {
    let mut app = app(grouping_entries());
    let selected = app.selected.clone();
    let mut terminal = terminal(120, 18);
    for mode in [
        GroupBy::Session,
        GroupBy::Cwd,
        GroupBy::Status,
        GroupBy::Agent,
        GroupBy::None,
    ] {
        render(&mut app, &mut terminal);
        let area = agent_area(&app, selected.as_ref().unwrap());
        assert!(app.handle_event(key(KeyCode::Char('G'))).is_none());
        assert_eq!(app.group_by, mode);
        assert_eq!(app.selected, selected);
        assert_no_hits(&app);
        assert!(app.handle_event(click(area.x, area.y)).is_none());
        assert_eq!(app.selected, selected);
        assert!(!app.navigating);
    }
}

#[test]
fn overlays_block_navigation_and_group_changes_and_close_without_quitting() {
    for overlay in ['?', 'w'] {
        for close in [KeyCode::Esc, KeyCode::Char('q'), KeyCode::Char(overlay)] {
            let mut app = grouped_app(grouping_entries(), GroupBy::Agent);
            let selected = app.selected.clone();
            let mut terminal = terminal(80, 8);
            render(&mut app, &mut terminal);
            let area = agent_area(&app, selected.as_ref().unwrap());
            app.handle_event(key(KeyCode::Char(overlay)));
            assert_no_hits(&app);
            render(&mut app, &mut terminal);
            for event in [
                key(KeyCode::Enter),
                key(KeyCode::Char('g')),
                key(KeyCode::Char('G')),
                key(KeyCode::Tab),
                key(KeyCode::Left),
                key(KeyCode::Char('/')),
                key(KeyCode::Char('a')),
                click(area.x, area.y),
            ] {
                assert!(app.handle_event(event).is_none());
                assert_eq!(app.selected, selected);
                assert_eq!(app.group_by, GroupBy::Agent);
                assert!(!app.navigating);
                assert_no_hits(&app);
            }
            assert!(app.handle_event(key(close)).is_none());
            assert!(!app.help && !app.details);
            assert_no_hits(&app);
            assert!(app.handle_event(click(area.x, area.y)).is_none());
            assert_eq!(
                navigation(app.handle_event(key(KeyCode::Enter))).id,
                selected.unwrap()
            );
        }
    }
}

#[test]
fn control_c_quits_from_filter_editing_and_both_overlays() {
    for state in ['/', '?', 'w'] {
        let mut app = many(1);
        app.connected = false;
        app.navigating = true;
        app.handle_event(key(KeyCode::Char(state)));
        for character in ['c', 'C'] {
            assert!(matches!(
                app.handle_event(Event::Key(KeyEvent::new(
                    KeyCode::Char(character),
                    KeyModifiers::CONTROL
                ))),
                Some(Action::Quit)
            ));
        }
    }
}

#[test]
fn help_scrolls_to_all_filter_fold_attention_and_status_safety_rules() {
    let mut app = many(1);
    app.handle_event(key(KeyCode::Char('?')));
    let mut terminal = terminal(80, 8);
    render(&mut app, &mut terminal);
    assert_eq!(app.details_scroll, 0);
    app.handle_event(key(KeyCode::PageDown));
    assert!(app.details_scroll > 0);
    app.handle_event(key(KeyCode::PageUp));
    assert_eq!(app.details_scroll, 0);
    wheel(&mut app, true);
    assert_eq!(app.details_scroll, 3);
    let text = overlay_text(&mut app, &mut terminal);
    for value in [
        "Collapsed headers are selectable",
        "expand only",
        "Enter applies without navigation",
        "restores the previous filter",
        "Blocked or Error",
        "Unrecognized states use Idle",
        "Idle fallback",
        "no-report source",
        "Blocked is not proof",
        "not whole-session completion",
    ] {
        assert!(text.contains(value), "missing {value}: {text}");
    }
    assert_no_hits(&app);
}

#[test]
fn attention_keys_visit_only_blocked_or_error_with_wraparound_and_expand_groups() {
    let mut app = grouped_app(grouping_entries(), GroupBy::Agent);
    let attention: Vec<_> = app
        .snapshot
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| matches!(entry.status, Status::Blocked | Status::Error))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(attention.len(), 2);
    for group in app.current_groups() {
        app.collapsed.insert(group.key);
    }
    app.selected = None;
    for &index in attention.iter().chain(attention.iter().take(1)) {
        assert!(app.handle_event(key(KeyCode::Char('a'))).is_none());
        assert_eq!(app.selected_index(), Some(index));
        assert!(!app.collapsed.contains(&app.selected_group().unwrap()));
        assert!(matches!(
            app.list_rows()[app.selected_row().unwrap()],
            ListRow::Agent(_)
        ));
        assert!(!app.navigating);
    }
    for &index in attention.iter().rev().chain(attention.iter().rev().take(1)) {
        app.handle_event(key(KeyCode::Char('A')));
        assert_eq!(app.selected_index(), Some(index));
    }
    apply_filter(&mut app, "status:error");
    app.handle_event(key(KeyCode::Char('a')));
    assert_eq!(
        app.snapshot.entries[app.selected_index().unwrap()].status,
        Status::Error
    );
    let selected = app.selected.clone();
    for query in [
        "status:done",
        "status:unknown",
        "status:working",
        "status:idle",
        "no-match",
    ] {
        apply_filter(&mut app, query);
        for character in ['a', 'A'] {
            app.handle_event(key(KeyCode::Char(character)));
            assert_eq!(app.selected, selected);
            assert!(app.message.contains("No matching Blocked or Error"));
        }
    }
}

#[test]
fn grouped_columns_remove_only_repeated_fields_and_single_groups_restore_them() {
    for (mode, hidden_index) in [
        (GroupBy::Status, 0),
        (GroupBy::Agent, 1),
        (GroupBy::Cwd, 2),
        (GroupBy::Session, 3),
    ] {
        for (width, height) in VIEW_SIZES {
            let mut app = grouped_app(grouping_entries(), mode);
            let mut terminal = terminal(width, height);
            let rows = render(&mut app, &mut terminal);
            assert!(app.current_groups().len() >= 2);
            assert!(!rows[1].contains(COLUMN_LABELS[hidden_index]));
            assert_independent_fields(&app, &terminal);
            let layout = app_columns(&app, app.hits[0].area);
            assert_eq!(layout.areas[hidden_index + 1].width, 0);
            if width == 120 {
                for (index, label) in COLUMN_LABELS.iter().enumerate() {
                    assert_eq!(rows[1].contains(label), index != hidden_index);
                }
            }
            apply_filter(&mut app, "agent:codex status:error");
            let rows = render(&mut app, &mut terminal);
            assert_eq!(app.current_groups().len(), 1);
            assert!(app.group_hits.is_empty());
            assert_independent_fields(&app, &terminal);
            let layout = app_columns(&app, app.hits[0].area);
            if width >= 80 {
                assert!(layout.areas[hidden_index + 1].width > 0);
            }
            if width == 120 {
                for label in COLUMN_LABELS {
                    assert!(rows[1].contains(label));
                }
            }
        }
    }
}

#[test]
fn selected_idle_status_contrasts_with_full_row_highlight_at_full_and_compact_widths() {
    for width in [100, 20] {
        let agent = entry(0, Status::Idle);
        let mut app = app(vec![agent.clone()]);
        let mut terminal = terminal(width, 10);
        render(&mut app, &mut terminal);
        let row = agent_area(&app, &agent.id);
        let layout = app_columns(&app, row);
        assert_eq!(layout.compact_status, width == 20);
        let status = layout.areas[1];
        let label = if layout.compact_status {
            "○"
        } else {
            "○ Idle"
        };
        let buffer = terminal.backend().buffer();
        assert_cell_text(buffer, status, label, false);
        for x in status.x..status.x + Line::raw(label).width() as u16 {
            let cell = &buffer[(x, row.y)];
            assert_ne!(cell.fg, cell.bg, "Idle status at width {width}, cell {x}");
        }
        for x in row.x..row.right() {
            assert_eq!(buffer[(x, row.y)].bg, Color::DarkGray);
        }
        assert_eq!(app.selected, Some(agent.id));
    }
}

#[test]
fn idle_fallback_and_details_identify_foreground_only_sources() {
    let mut agent = entry(0, Status::Idle);
    agent.records = 0;
    let mut app = app(vec![agent]);
    let mut terminal = terminal(100, 10);
    let rows = render(&mut app, &mut terminal);
    let status = app_columns(&app, app.hits[0].area).areas[1];
    assert!(rows[usize::from(status.y)].contains("○ Idle"));
    assert_eq!(
        terminal.backend().buffer()[(status.x, status.y)].fg,
        Color::Gray
    );
    assert!(!rows[9].contains("attention"));
    assert!(!rows[9].contains("working"));
    app.handle_event(key(KeyCode::Char('w')));
    assert!(
        overlay_text(&mut app, &mut terminal)
            .contains("Foreground identity only; no status report")
    );
    app.handle_event(key(KeyCode::Esc));
    app.snapshot.entries[0].records = 1;
    app.handle_event(key(KeyCode::Char('w')));
    assert!(overlay_text(&mut app, &mut terminal).contains("Reported by Rex program_status"));
}

#[test]
fn filter_group_fold_resize_and_refresh_invalidate_both_mouse_maps_and_queued_clicks() {
    for change in [
        "filter", "cancel", "clear", "group", "reverse", "fold", "resize", "refresh", "wheel",
    ] {
        let mut app = grouped_app(grouping_entries(), GroupBy::Agent);
        let mut terminal = terminal(120, 8);
        if change == "clear" {
            apply_filter(&mut app, "cwd:/work");
        }
        if change == "cancel" {
            app.handle_event(key(KeyCode::Char('/')));
        }
        render(&mut app, &mut terminal);
        let agent = app.hits[0].area;
        let header = app.group_hits[0].area;
        match change {
            "filter" => {
                app.handle_event(key(KeyCode::Char('/')));
            }
            "cancel" | "clear" => {
                app.handle_event(key(KeyCode::Esc));
            }
            "group" => {
                app.handle_event(key(KeyCode::Char('g')));
            }
            "reverse" => {
                app.handle_event(key(KeyCode::Char('G')));
            }
            "fold" => {
                app.handle_event(key(KeyCode::Left));
            }
            "resize" => {
                app.handle_event(Event::Resize(30, 5));
            }
            "refresh" => app.replace_snapshot(app.snapshot.clone()),
            "wheel" => wheel(&mut app, true),
            _ => unreachable!(),
        }
        let selected = app.selected.clone();
        let collapsed = app.collapsed.clone();
        assert_no_hits(&app);
        for area in [agent, header, agent] {
            assert!(
                app.handle_event(click(area.x, area.y)).is_none(),
                "{change}"
            );
            assert_eq!(app.selected, selected);
            assert_eq!(app.collapsed, collapsed);
            assert!(!app.navigating);
        }
    }
}

#[test]
fn queued_agent_or_header_clicks_cannot_use_a_consumed_map() {
    for header in [false, true] {
        let mut app = grouped_app(grouping_entries(), GroupBy::Agent);
        render(&mut app, &mut terminal(120, 18));
        let area = if header {
            app.group_hits[0].area
        } else {
            app.hits[0].area
        };
        let action = app.handle_event(click(area.x, area.y));
        if header {
            assert!(action.is_none());
        } else {
            navigation(action);
        }
        let collapsed = app.collapsed.clone();
        let selected = app.selected.clone();
        app.navigating = false;
        assert_no_hits(&app);
        assert!(app.handle_event(click(area.x, area.y)).is_none());
        assert_eq!(app.collapsed, collapsed);
        assert_eq!(app.selected, selected);
        assert!(!app.navigating);
    }
}

#[test]
fn sixty_entries_keep_exact_keyboard_mouse_targets_through_filter_group_resize_and_refresh() {
    let entries: Vec<_> = (0..60)
        .map(|index| {
            let mut entry = entry(index, Status::Working);
            entry.id.agent = ["claude-code", "codex", "pi"][index % 3].into();
            entry.name = "same display name".into();
            entry.working_dir = Some(format!("/work/project-{}", index % 4));
            entry
        })
        .collect();
    for mode in [
        GroupBy::None,
        GroupBy::Agent,
        GroupBy::Status,
        GroupBy::Cwd,
        GroupBy::Session,
    ] {
        let mut app = grouped_app(entries.clone(), mode);
        let mut terminal = terminal(80, 8);
        render(&mut app, &mut terminal);
        wheel(&mut app, true);
        render(&mut app, &mut terminal);
        let scroll = app.scroll;
        app.replace_snapshot(app.snapshot.clone());
        render(&mut app, &mut terminal);
        assert_eq!(app.scroll, scroll);
        let selected = app.selected.clone();
        assert!(
            !app.hits
                .iter()
                .any(|hit| Some(&hit.entry.id) == selected.as_ref())
        );
        apply_filter(&mut app, "cwd:project-2 status:working");
        assert_eq!(matching_indices(&app.snapshot, &app.query).len(), 15);
        for _ in 0..60 {
            app.handle_event(key(KeyCode::Down));
        }
        let mut expected = app.snapshot.entries[app.selected_index().unwrap()].clone();
        for (width, height) in VIEW_SIZES.into_iter().chain([(35, 5)]) {
            expected.target.window = format!("refreshed-exact-window-{width}-{height}");
            let mut refreshed = app.snapshot.clone();
            for entry in &mut refreshed.entries {
                if entry.id == expected.id {
                    *entry = expected.clone();
                }
            }
            refreshed.entries.reverse();
            let stale = app.hits.first().map(|hit| hit.area);
            app.replace_snapshot(refreshed);
            app.handle_event(Event::Resize(width, height));
            terminal.backend_mut().resize(width, height);
            terminal.resize(Rect::new(0, 0, width, height)).unwrap();
            assert_no_hits(&app);
            if let Some(area) = stale {
                assert!(app.handle_event(click(area.x, area.y)).is_none());
                assert_eq!(app.selected, Some(expected.id.clone()));
            }
            render(&mut app, &mut terminal);
            assert_independent_fields(&app, &terminal);
            if height == 5 {
                assert_eq!(app.hits.len(), 1);
            }
            let by_key = navigation(app.handle_event(key(KeyCode::Enter)));
            app.navigating = false;
            render(&mut app, &mut terminal);
            let by_mouse = navigation(click_agent(&mut app, &expected.id));
            assert_eq!(by_key, expected, "{mode:?}/{width}x{height}");
            assert_eq!(by_mouse, expected, "{mode:?}/{width}x{height}");
            app.navigating = false;
            render(&mut app, &mut terminal);
        }
    }
}

#[test]
fn short_fields_are_content_fit_left_aligned_and_do_not_expand_with_the_terminal() {
    let mut short = entry(0, Status::Working);
    short.name = "pi".into();
    short.working_dir = Some("/x".into());
    short.location = Location {
        session: "work".into(),
        tab: "one".into(),
        pane: "two".into(),
    };
    let measured = measure_columns([&short], None);
    let natural_width = 2 + measured.iter().sum::<u16>() + 2 * 5;
    let mut natural_positions = None;
    for (width, height) in VIEW_SIZES {
        let mut app = app(vec![short.clone()]);
        let mut terminal = terminal(width, height);
        let rows = render(&mut app, &mut terminal);
        assert_independent_fields(&app, &terminal);
        assert!(!rows[1].contains("Location"));
        assert!(!rows[3].contains(" / "));
        let layout = app_columns(&app, app.hits[0].area);
        let fields = &layout.areas[1..];
        if natural_width <= width {
            for (index, label) in COLUMN_LABELS.iter().enumerate() {
                assert!(rows[1].contains(label), "{width}: {label}");
                assert_eq!(fields[index].width, measured[index]);
            }
            let positions = layout.areas.map(|area| (area.x, area.width));
            assert_eq!(*natural_positions.get_or_insert(positions), positions);
            assert_eq!(layout.areas[6].right(), natural_width);
            for x in natural_width..width {
                assert_eq!(terminal.backend().buffer()[(x, 3)].symbol(), " ");
            }
        }
        if fields[3].width == 0 {
            assert_eq!(fields[4].width, 0);
            assert_eq!(fields[5].width, 0);
        }
        if fields[4].width == 0 {
            assert_eq!(fields[5].width, 0);
        }
        assert!(fields[0].width > 0);
        assert!(fields[1].width > 0);
        assert!(fields[2].width > 0);
    }
}

#[test]
fn unicode_and_long_paths_stay_inside_independent_columns_at_requested_dimensions() {
    let mut long = entry(0, Status::Blocked);
    let unicode = "界e\u{301}👩🏽/".repeat(20);
    long.name = format!("\x1b\n\u{202e}{unicode}AGENT");
    long.working_dir = Some(format!("/work/{unicode}project"));
    long.location = Location {
        session: format!("{unicode}SESSION").into(),
        tab: format!("{unicode}TAB").into(),
        pane: format!("{unicode}PANE").into(),
    };
    let expected = long.clone();
    for (width, height) in VIEW_SIZES {
        let mut app = app(vec![long.clone(), entry(1, Status::Working)]);
        let mut terminal = terminal(width, height);
        render(&mut app, &mut terminal);
        assert_independent_fields(&app, &terminal);
        for cell in &terminal.backend().buffer().content {
            assert_eq!(plain(cell.symbol()), cell.symbol());
            assert!(!cell.symbol().starts_with('\u{301}'));
        }
        let area = app_columns(&app, agent_area(&app, &expected.id)).areas[3];
        let clipped = clip_text(expected.working_dir.as_deref().unwrap(), area.width, true);
        assert!(clipped.starts_with('…'));
        if area.width >= 9 {
            assert!(clipped.ends_with("/project"));
        }
        assert_eq!(navigation(app.handle_event(key(KeyCode::Enter))), expected);
        app.navigating = false;
        render(&mut app, &mut terminal);
        assert_eq!(navigation(click_agent(&mut app, &expected.id)), expected);
    }
}

#[test]
fn columns_measure_collapsed_and_offscreen_matching_members_not_visible_rows() {
    let entries: Vec<_> = (0..60)
        .map(|index| {
            let mut entry = entry(index, Status::Working);
            entry.id.agent = if index < 30 { "codex" } else { "pi" }.into();
            entry.name = entry.id.agent.clone();
            entry.working_dir = Some("/x".into());
            entry.location = Location {
                session: "s".into(),
                tab: "t".into(),
                pane: "p".into(),
            };
            if index == 0 {
                entry.name = "long-agent-name-for-measurement".into();
                entry.working_dir = Some(format!("/work/{}project", "long/".repeat(15)));
                entry.location = Location {
                    session: "long-session-for-measurement".into(),
                    tab: "long-tab-for-measurement".into(),
                    pane: "long-pane-for-measurement".into(),
                };
            }
            entry
        })
        .collect();
    for (width, height) in VIEW_SIZES {
        let mut app = grouped_app(entries.clone(), GroupBy::Agent);
        let mut terminal = terminal(width, height);
        render(&mut app, &mut terminal);
        let baseline = app_columns(&app, app.hits[0].area)
            .areas
            .map(|area| (area.x, area.width));
        assert_independent_fields(&app, &terminal);
        // Collapse the group that supplies all maximum widths, with a non-first member selected.
        app.select_index(1);
        let selected = app.selected.clone();
        app.handle_event(key(KeyCode::Left));
        render(&mut app, &mut terminal);
        assert_eq!(app.selected, selected);
        assert!(!app.hits.iter().any(|hit| hit.entry.id.agent == "codex"));
        assert_independent_fields(&app, &terminal);
        assert_eq!(
            app_columns(&app, app.hits[0].area)
                .areas
                .map(|area| (area.x, area.width)),
            baseline
        );
        wheel(&mut app, true);
        render(&mut app, &mut terminal);
        assert!(app.scroll > 0);
        assert_independent_fields(&app, &terminal);
        assert_eq!(
            app_columns(&app, app.hits[0].area)
                .areas
                .map(|area| (area.x, area.width)),
            baseline
        );
        let scroll = app.scroll;
        app.replace_snapshot(app.snapshot.clone());
        render(&mut app, &mut terminal);
        assert_eq!(app.scroll, scroll);
        assert_independent_fields(&app, &terminal);
        assert_eq!(
            app_columns(&app, app.hits[0].area)
                .areas
                .map(|area| (area.x, area.width)),
            baseline
        );
        // Filtering removes the maximum-width member from measurement.
        apply_filter(&mut app, "agent:pi");
        let rows = render(&mut app, &mut terminal);
        assert!(app.group_hits.is_empty());
        assert_independent_fields(&app, &terminal);
        if width >= 60 {
            for label in COLUMN_LABELS {
                assert!(rows[1].contains(label));
            }
        }
    }
}

#[test]
fn independent_column_layout_samples() {
    // cargo test --locked --lib tui::tests::independent_column_layout_samples -- --nocapture
    for mode in [
        GroupBy::None,
        GroupBy::Agent,
        GroupBy::Cwd,
        GroupBy::Session,
    ] {
        for (width, height) in VIEW_SIZES {
            let mut app = grouped_app(grouping_entries(), mode);
            app.select_index(3);
            if mode != GroupBy::None {
                app.handle_event(key(KeyCode::Left));
            }
            let mut terminal = terminal(width, height);
            let rows = render(&mut app, &mut terminal);
            println!("{mode:?} ({width} × {height}):\n{}", rows.join("\n"));
            assert!(!rows[1].contains("Location"));
            assert_independent_fields(&app, &terminal);
            if mode == GroupBy::None {
                assert_eq!(app.hits.len(), 6);
                assert!(app.group_hits.is_empty());
            } else {
                assert_eq!(app.hits.len(), 4);
                assert_eq!(app.group_hits.len(), 3);
                let header = app
                    .group_hits
                    .iter()
                    .find(|hit| app.collapsed.contains(&hit.key))
                    .unwrap()
                    .area;
                assert!(rows[usize::from(header.y)].starts_with('▸'));
                for x in 0..width {
                    assert_eq!(
                        terminal.backend().buffer()[(x, header.y)].bg,
                        Color::DarkGray
                    );
                }
            }
        }
    }
}

#[test]
fn long_header_labels_keep_matching_counts_visible_at_narrow_widths() {
    for mode in [GroupBy::Agent, GroupBy::Cwd, GroupBy::Session] {
        let mut entries = grouping_entries();
        let prefix = "界e\u{301}-long-label/".repeat(20);
        for entry in &mut entries {
            entry.id.agent = format!("{prefix}{}", entry.id.agent);
            if let Some(path) = &mut entry.working_dir {
                *path = format!("/{prefix}{path}");
            }
            entry.location.session = format!("{prefix}{}", entry.location.session).into();
        }
        let mut app = grouped_app(entries, mode);
        for width in [1, 2, 6, 8, 16, 32, 64] {
            let rows = render(&mut app, &mut terminal(width, 18));
            assert_eq!(app.group_hits.len(), 3);
            for hit in &app.group_hits {
                let text = rows[usize::from(hit.area.y)].trim_end();
                if width >= 6 {
                    assert!(text.ends_with("(2)"), "{mode:?}/{width}: {text}");
                } else {
                    assert_eq!(text, "2");
                }
            }
        }
    }
}

#[test]
fn navigation_diagnostics_are_complete_in_details_but_do_not_create_footer_notices() {
    let mut app = many(1);
    app.client = Some("client:12345678-1234-1234-1234-123456789abc".into());
    app.diagnostic = format!(
        "Navigation accepted: {} \x1b\n\u{202e}RECEIPT-END",
        "exact-receipt-id/".repeat(40)
    );
    let diagnostic = app.diagnostic.clone();
    let mut terminal = terminal(100, 10);
    let rows = render(&mut app, &mut terminal);
    assert!(rows[9].contains("Connected"));
    for value in [
        "Notice",
        "12345678",
        "exact-receipt-id",
        "0 warnings",
        "Warnings: 0",
    ] {
        assert!(!rows[9].contains(value));
    }
    assert_eq!(app.footer(100).1, Color::Green);
    app.handle_event(key(KeyCode::Char('w')));
    let text = overlay_text(&mut app, &mut terminal);
    assert!(text.contains(&display_text(&format!("Last navigation: {diagnostic}"))));
    assert!(text.contains(app.client.as_deref().unwrap()));
    assert_eq!(app.diagnostic, diagnostic);
    app.handle_event(key(KeyCode::Char('q')));
    assert!(!app.details);
    assert_eq!(app.footer(100).1, Color::Green);
}

#[test]
fn details_scroll_preserves_all_sanitized_unicode_values_without_changing_exact_targets() {
    let mut selected = entry(0, Status::Error);
    let long = "界e\u{301}🦕-value/".repeat(25);
    selected.name = format!("Agent-{long}\x1b\n\u{202e}END");
    selected.location.session = format!("Session-{long}\rEND").into();
    selected.location.tab = format!("Tab-{long}\u{2066}END").into();
    selected.location.pane = format!("Pane-{long}\nEND").into();
    selected.working_dir = Some(format!("/work/{long}\x1bEND"));
    selected.target.session = format!("session:{long}\u{202e}END");
    selected.target.window = format!("window:{long}\rEND");
    selected.target.block = format!("block:{long}\nEND");
    let expected = selected.clone();
    let mut app = app(vec![selected]);
    apply_filter(&mut app, "status:error");
    app.handle_event(key(KeyCode::Char('w')));
    let mut terminal = terminal(80, 8);
    let text = overlay_text(&mut app, &mut terminal);
    let compact = |value: &str| {
        value
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
    };
    for (label, value) in [
        ("Agent", expected.name.as_str()),
        ("Session", expected.location.session.as_ref()),
        ("Tab", expected.location.tab.as_ref()),
        ("Pane", expected.location.pane.as_ref()),
        (
            "Working directory",
            expected.working_dir.as_deref().unwrap(),
        ),
        ("Session ID", expected.target.session.as_str()),
        ("Tab ID", expected.target.window.as_str()),
        ("Pane ID", expected.target.block.as_str()),
    ] {
        let sanitized = display_text(&format!("{label}: {value}"));
        assert!(
            compact(&text).contains(&compact(&sanitized)),
            "missing full {label}"
        );
    }
    assert_eq!(display_text(&text), text);
    app.handle_event(key(KeyCode::Esc));
    assert_eq!(navigation(app.handle_event(key(KeyCode::Enter))), expected);
}

#[test]
fn tiny_help_and_filter_views_render_without_targets_outside_the_body() {
    for state in ['?', '/'] {
        for width in [0, 1, 2, 8, 17, 80] {
            for height in [0, 1, 2, 3, 4, 5, 24] {
                let mut app = grouped_app(grouping_entries(), GroupBy::Agent);
                app.handle_event(key(KeyCode::Char(state)));
                if state == '/' {
                    app.handle_event(Event::Paste("界e\u{301}".repeat(50)));
                }
                render(&mut app, &mut terminal(width, height));
                assert_no_hits(&app);
                assert!(!app.navigating);
            }
        }
    }
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
