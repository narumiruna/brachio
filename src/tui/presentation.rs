//! Pure text and layout helpers. Display values do not change model data.

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
};

use crate::model::{Entry, Status, display_text};

pub(super) fn clip_text(value: &str, width: u16, keep_end: bool) -> String {
    let text = display_text(value);
    if width == 0 {
        return String::new();
    }
    if Line::raw(text.as_str()).width() <= usize::from(width) {
        return text;
    }
    let span = Span::raw(text.as_str());
    let graphemes: Vec<_> = span
        .styled_graphemes(Style::default())
        .map(|g| g.symbol)
        .collect();
    let mut parts = Vec::new();
    let mut used = 0;
    for index in 0..graphemes.len() {
        let index = if keep_end {
            graphemes.len() - 1 - index
        } else {
            index
        };
        let grapheme = graphemes[index];
        let size = Span::raw(grapheme).width();
        if used + size > usize::from(width - 1) {
            break;
        }
        parts.push(grapheme);
        used += size;
    }
    if keep_end {
        format!("…{}", parts.into_iter().rev().collect::<String>())
    } else {
        format!("{}…", parts.concat())
    }
}

pub(super) fn wrap_details(value: &str, width: u16) -> Vec<Line<'static>> {
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

pub(super) fn home_path(path: &str, home: Option<&str>) -> String {
    let Some(home) = home.filter(|home| home.starts_with('/')) else {
        return path.into();
    };
    if !path.starts_with('/') {
        return path.into();
    }
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return if path == "/" {
            "~".into()
        } else {
            format!("~{path}")
        };
    }
    match path.strip_prefix(home) {
        Some("") => "~".into(),
        Some(suffix) if suffix.starts_with('/') => format!("~{suffix}"),
        _ => path.into(),
    }
}

pub(super) const COLUMN_LABELS: [&str; 6] = [
    "Status",
    "Agent",
    "Working directory",
    "Session",
    "Tab",
    "Pane",
];

/// Measure display cells independently of the terminal width.
pub(super) fn measure_columns<'a>(
    entries: impl IntoIterator<Item = &'a Entry>,
    home: Option<&str>,
) -> [u16; 6] {
    const CAPS: [usize; 6] = [9, 20, 48, 24, 20, 24];
    let mut measured = COLUMN_LABELS.map(|label| Line::raw(label).width());
    for entry in entries {
        let cwd = home_path(entry.working_dir.as_deref().unwrap_or("Unknown"), home);
        let fields = [
            status_text(entry.status, false),
            entry.name.as_str(),
            cwd.as_str(),
            entry.location.session.as_ref(),
            entry.location.tab.as_ref(),
            entry.location.pane.as_ref(),
        ];
        for (index, field) in fields.into_iter().enumerate() {
            measured[index] = measured[index].max(Line::raw(display_text(field)).width());
        }
    }
    std::array::from_fn(|index| measured[index].min(CAPS[index]) as u16)
}

#[derive(Debug)]
pub(super) struct ColumnLayout {
    pub areas: [Rect; 7],
    pub compact_status: bool,
}

/// Keep natural widths and use two cells between visible data columns.
/// Remove Pane, Tab, then Session before reducing the essential columns.
pub(super) fn columns(area: Rect, measured: [u16; 6], hidden: [bool; 6]) -> ColumnLayout {
    fn cost(widths: [u16; 6]) -> u32 {
        let count = widths.iter().filter(|&&width| width > 0).count() as u32;
        widths.into_iter().map(u32::from).sum::<u32>() + 2 * count.saturating_sub(1)
    }

    let width = area.right().saturating_sub(area.x);
    let marker = if width >= 3 { 2 } else { 0 };
    let available = u32::from(width - marker);
    let mut desired: [u16; 6] =
        std::array::from_fn(|index| if hidden[index] { 0 } else { measured[index] });
    let mut widths =
        std::array::from_fn(|index| desired[index].min([u16::MAX, 12, 10, 7, 5, 6][index]));
    for index in [5, 4, 3] {
        if cost(widths) <= available {
            break;
        }
        desired[index] = 0;
        widths[index] = 0;
    }

    let compact_status = cost(widths) > available;
    if compact_status {
        desired[0] = desired[0].min(1);
        widths[0] = desired[0];
        widths[1] = desired[1].min(3);
        widths[2] = desired[2].min(4);
        for index in [2, 1] {
            if cost(widths) <= available {
                break;
            }
            desired[index] = 0;
            widths[index] = 0;
        }
        // At zero cells even the status symbol cannot fit.
        widths[0] = widths[0].min(available as u16);
    }

    let mut remaining = available - cost(widths);
    for index in 0..6 {
        if widths[index] == 0 {
            continue;
        }
        let extra = u32::from(desired[index] - widths[index]).min(remaining) as u16;
        widths[index] += extra;
        remaining -= u32::from(extra);
    }

    let mut x = area.x;
    let mut has_data = false;
    let areas = std::array::from_fn(|index| {
        let column_width = if index == 0 {
            marker
        } else {
            widths[index - 1]
        };
        if index != 0 && column_width > 0 {
            if has_data {
                x += 2;
            }
            has_data = true;
        }
        let column = Rect::new(x, area.y, column_width, area.height);
        x += column_width;
        column
    });
    ColumnLayout {
        areas,
        compact_status,
    }
}

pub(super) fn status_text(status: Status, compact: bool) -> &'static str {
    match (status, compact) {
        (Status::Blocked, true) => "!",
        (Status::Error, true) => "×",
        (Status::Done, true) => "✓",
        (Status::Working, true) => "●",
        (Status::Idle, true) => "○",
        (Status::Blocked, false) => "! Blocked",
        (Status::Error, false) => "× Error",
        (Status::Done, false) => "✓ Done",
        (Status::Working, false) => "● Working",
        (Status::Idle, false) => "○ Idle",
    }
}

pub(super) fn status_color(status: Status) -> Color {
    match status {
        Status::Blocked => Color::Yellow,
        Status::Error => Color::Red,
        Status::Done => Color::Green,
        Status::Working => Color::Cyan,
        Status::Idle => Color::Gray,
    }
}

/// Skip empty or unfit parts. Only the first nonempty part can be clipped.
pub(super) fn compact_line(parts: &[&str], width: u16) -> String {
    let mut line = String::new();
    let mut used = 0;
    let limit = usize::from(width);
    for part in parts {
        let part = display_text(part);
        if part.is_empty() {
            continue;
        }
        let size = Line::raw(part.as_str()).width();
        if line.is_empty() {
            if size > limit {
                return clip_text(&part, width, false);
            }
            line.push_str(&part);
            used = size;
        } else if used + 3 + size <= limit {
            line.push_str(" · ");
            line.push_str(&part);
            used += 3 + size;
        }
    }
    line
}

pub(super) fn shortcuts(width: u16, grouped: bool) -> String {
    let mut parts = vec![
        "↑↓ Select",
        "Enter Jump",
        "g/G Group",
        "/ Filter",
        "? Help",
        "q Quit",
    ];
    if grouped {
        parts.extend(["Tab Groups", "←/→ Fold"]);
    }
    parts.push("w Details");
    compact_line(&parts, width)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUSES: [Status; 5] = [
        Status::Blocked,
        Status::Error,
        Status::Done,
        Status::Working,
        Status::Idle,
    ];

    fn text_width(value: &str) -> usize {
        Line::raw(value).width()
    }

    fn entry() -> Entry {
        use crate::model::{EntryId, Location, Target};

        Entry {
            id: EntryId {
                session: "session-id".into(),
                block: "pane-id".into(),
                agent: "pi".into(),
            },
            target: Target {
                session: "session-id".into(),
                window: "tab-id".into(),
                block: "pane-id".into(),
            },
            name: "pi".into(),
            status: Status::Working,
            location: Location {
                session: "work".into(),
                tab: "one".into(),
                pane: "main".into(),
            },
            working_dir: Some("/home/user/project".into()),
            records: 1,
        }
    }

    #[test]
    fn measurements_include_headers_and_full_status_labels() {
        assert_eq!(measure_columns([], None), [6, 5, 17, 7, 3, 4]);
        for status in STATUSES {
            let mut item = entry();
            item.status = status;
            let measured = measure_columns([&item], Some("/home/user"));
            assert_eq!(
                measured[0],
                6.max(text_width(status_text(status, false))) as u16
            );
            assert_eq!(&measured[1..], &[5, 17, 7, 3, 4]);
        }
        let mut first = entry();
        first.name = "long-agent-name".into();
        let mut second = entry();
        second.location.session = "long-session-name".into();
        assert_eq!(
            measure_columns([&first, &second], Some("/home/user")),
            [9, 15, 17, 17, 3, 4]
        );
    }

    #[test]
    fn measurements_use_sanitized_unicode_terminal_cells() {
        let mut item = entry();
        item.name = "\x1b界e\u{301}界界\n\u{202e}".into();
        item.working_dir = Some(format!("/home/user/{}", "界".repeat(9)));
        item.location.session = "界界界界".into();
        item.location.tab = "e\u{301}e\u{301}e\u{301}e\u{301}".into();
        item.location.pane = "👍🏽👍🏽👍🏽\t".into();
        assert_eq!(
            measure_columns([&item], Some("/home/user")),
            [9, 7, 20, 8, 4, 6]
        );
    }

    #[test]
    fn measurements_cap_each_field_independently() {
        let mut item = entry();
        item.name = "a".repeat(80);
        item.working_dir = Some("/".repeat(100));
        item.location.session = "s".repeat(80).into();
        item.location.tab = "t".repeat(80).into();
        item.location.pane = "p".repeat(80).into();
        assert_eq!(measure_columns([&item], None), [9, 20, 48, 24, 20, 24]);
    }

    #[test]
    fn measurements_abbreviate_home_only_at_path_boundaries() {
        for (path, home, expected) in [
            ("/home/user/a-long-project", Some("/home/user"), 17),
            ("/home/user/a-long-project", Some("/home/user/"), 17),
            ("/home/user2/a-long-project", Some("/home/user"), 26),
            ("/home/user-old/a-long-project", Some("/home/user"), 29),
            ("/home/user/a-long-project", Some("home/user"), 25),
            ("home/user/a-long-project", Some("/home/user"), 24),
            ("/home/user/a-long-project", None, 25),
        ] {
            let mut item = entry();
            item.working_dir = Some(path.into());
            assert_eq!(measure_columns([&item], home)[2], expected, "{path}");
        }
        let mut item = entry();
        item.working_dir = None;
        assert_eq!(measure_columns([&item], None)[2], 17);
    }

    #[test]
    fn short_content_keeps_six_columns_without_spreading() {
        let measured = measure_columns([&entry()], Some("/home/user"));
        let baseline = columns(Rect::new(7, 3, 120, 1), measured, [false; 6]);
        for width in [80, 100, 120, 200] {
            let layout = columns(Rect::new(7, 3, width, 1), measured, [false; 6]);
            assert_eq!(layout.areas, baseline.areas);
            assert!(!layout.compact_status);
            assert_eq!(layout.areas[0].width, 2);
            assert_eq!(layout.areas[1].x, layout.areas[0].right());
            for (index, &measured_width) in measured.iter().enumerate() {
                assert_eq!(layout.areas[index + 1].width, measured_width);
                if index > 0 {
                    assert_eq!(layout.areas[index + 1].x, layout.areas[index].right() + 2);
                }
            }
        }
    }

    #[test]
    fn location_fields_have_independent_widths_and_gaps() {
        let mut item = entry();
        item.location.session = "session-with-name".into();
        item.location.tab = "tab-name".into();
        item.location.pane = "pane-name".into();
        let measured = measure_columns([&item], Some("/home/user"));
        let layout = columns(Rect::new(0, 0, 120, 1), measured, [false; 6]);
        assert_eq!(layout.areas[4].width, 17);
        assert_eq!(layout.areas[5].width, 8);
        assert_eq!(layout.areas[6].width, 9);
        assert_eq!(layout.areas[5].x, layout.areas[4].right() + 2);
        assert_eq!(layout.areas[6].x, layout.areas[5].right() + 2);
    }

    #[test]
    fn masks_remove_columns_and_gaps_without_leaving_holes() {
        let measured = [9, 20, 48, 24, 20, 24];
        for mask in 0..64 {
            let hidden = std::array::from_fn(|index| mask & (1 << index) != 0);
            let layout = columns(Rect::new(7, 3, 200, 1), measured, hidden);
            let mut right = layout.areas[0].right();
            let mut has_data = false;
            for index in 0..6 {
                let column = layout.areas[index + 1];
                if hidden[index] {
                    assert_eq!(column.width, 0);
                    assert_eq!(column.x, right);
                } else {
                    assert_eq!(column.width, measured[index]);
                    assert_eq!(column.x, right + if has_data { 2 } else { 0 });
                    right = column.right();
                    has_data = true;
                }
            }
        }
        let hidden_status = columns(
            Rect::new(0, 0, 40, 1),
            measured,
            [true, false, false, true, true, true],
        );
        assert_eq!(hidden_status.areas[1].width, 0);
        assert_eq!(hidden_status.areas[2].x, 2);
        assert!(!hidden_status.compact_status);
    }

    #[test]
    fn secondary_columns_disappear_only_when_minima_do_not_fit() {
        let measured = [9, 20, 48, 24, 20, 24];
        for (width, visible) in [
            (61, [true, true, true, true, true, true]),
            (60, [true, true, true, true, true, false]),
            (53, [true, true, true, true, true, false]),
            (52, [true, true, true, true, false, false]),
            (46, [true, true, true, true, false, false]),
            (45, [true, true, true, false, false, false]),
            (40, [true, true, true, false, false, false]),
            (37, [true, true, true, false, false, false]),
        ] {
            let layout = columns(Rect::new(0, 0, width, 1), measured, [false; 6]);
            assert!(!layout.compact_status, "{width}: {layout:?}");
            for (index, visible) in visible.into_iter().enumerate() {
                assert_eq!(layout.areas[index + 1].width > 0, visible, "{width}");
            }
        }
        let layout = columns(Rect::new(0, 0, 40, 1), measured, [false; 6]);
        assert_eq!(layout.areas.map(|area| area.width), [2, 9, 15, 10, 0, 0, 0]);
    }

    #[test]
    fn compact_fallback_keeps_one_status_cell_and_hides_cwd_then_agent() {
        let measured = [9, 20, 48, 24, 20, 24];
        for (width, expected) in [
            (36, [2, 1, 20, 9, 0, 0, 0]),
            (14, [2, 1, 3, 4, 0, 0, 0]),
            (13, [2, 1, 8, 0, 0, 0, 0]),
            (8, [2, 1, 3, 0, 0, 0, 0]),
            (7, [2, 1, 0, 0, 0, 0, 0]),
            (3, [2, 1, 0, 0, 0, 0, 0]),
            (2, [0, 1, 0, 0, 0, 0, 0]),
            (1, [0, 1, 0, 0, 0, 0, 0]),
            (0, [0, 0, 0, 0, 0, 0, 0]),
        ] {
            let layout = columns(Rect::new(0, 0, width, 1), measured, [false; 6]);
            assert!(layout.compact_status, "{width}: {layout:?}");
            assert_eq!(layout.areas.map(|area| area.width), expected, "{width}");
        }
    }

    #[test]
    fn content_changes_visibility_at_the_same_terminal_width() {
        let short = measure_columns([&entry()], Some("/home/user"));
        let long = [9, 20, 48, 24, 20, 24];
        let area = Rect::new(0, 0, 54, 1);
        let short_layout = columns(area, short, [false; 6]);
        let long_layout = columns(area, long, [false; 6]);
        assert!(short_layout.areas.iter().all(|column| column.width > 0));
        assert_eq!(long_layout.areas[6].width, 0);
        assert!(long_layout.areas[5].width > 0);
    }

    #[test]
    fn remaining_space_uses_field_priority_and_never_exceeds_measurements() {
        let measured = [9, 20, 48, 24, 20, 24];
        for (width, expected) in [
            (61, [2, 9, 12, 10, 7, 5, 6]),
            (69, [2, 9, 20, 10, 7, 5, 6]),
            (107, [2, 9, 20, 48, 7, 5, 6]),
            (124, [2, 9, 20, 48, 24, 5, 6]),
            (139, [2, 9, 20, 48, 24, 20, 6]),
            (200, [2, 9, 20, 48, 24, 20, 24]),
        ] {
            let layout = columns(Rect::new(0, 0, width, 1), measured, [false; 6]);
            assert_eq!(layout.areas.map(|area| area.width), expected, "{width}");
        }
    }

    #[test]
    fn layouts_stay_inside_bounds_for_all_widths_offsets_and_masks() {
        for measured in [
            [9, 20, 48, 24, 20, 24],
            [6, 5, 17, 7, 3, 4],
            [0; 6],
            [u16::MAX; 6],
        ] {
            for width in 0..=200 {
                for x in [0, 7, u16::MAX - 20, u16::MAX - 1, u16::MAX] {
                    for mask in 0..64 {
                        let hidden = std::array::from_fn(|index| mask & (1 << index) != 0);
                        // Also check Rect values that were not clamped by Rect::new.
                        let area = Rect {
                            x,
                            y: 3,
                            width,
                            height: 2,
                        };
                        let layout = columns(area, measured, hidden);
                        let mut right = area.x;
                        let mut has_data = false;
                        for (index, column) in layout.areas.into_iter().enumerate() {
                            assert!(column.x >= right, "{area:?}: {layout:?}");
                            assert!(column.right() <= area.right(), "{area:?}: {layout:?}");
                            assert_eq!(column.y, area.y);
                            assert_eq!(column.height, area.height);
                            if index > 0 {
                                if hidden[index - 1] {
                                    assert_eq!(column.width, 0);
                                }
                                assert!(column.width <= measured[index - 1]);
                                if column.width > 0 {
                                    assert_eq!(column.x - right, if has_data { 2 } else { 0 });
                                    has_data = true;
                                } else {
                                    assert_eq!(column.x, right);
                                }
                            }
                            right = column.right();
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn clipping_preserves_graphemes_and_removes_controls() {
        assert_eq!(clip_text("界e\u{301}界", 4, false), "界e\u{301}…");
        assert_eq!(clip_text("界e\u{301}界", 4, true), "…e\u{301}界");
        assert_eq!(clip_text("👩🏽abc", 3, false), "👩🏽…");
        assert_eq!(clip_text("a\x1b\n\u{202e}b", 20, false), "ab");
        assert_eq!(clip_text("anything", 1, true), "…");
        assert_eq!(clip_text("anything", 0, false), "");
        for width in 0..=20 {
            for keep_end in [false, true] {
                let clipped = clip_text("界e\u{301}/👩🏽/project", width, keep_end);
                assert!(text_width(&clipped) <= usize::from(width));
                assert!(!clipped.starts_with("…\u{301}"));
            }
        }
    }

    #[test]
    fn details_wrap_without_splitting_graphemes_or_losing_text() {
        let lines = wrap_details("界e\u{301}abc", 3);
        assert_eq!(lines, vec![Line::raw("界e\u{301}"), Line::raw("abc")]);
        assert_eq!(wrap_details("", 5), vec![Line::raw("")]);
        // Preserve the existing behavior for indivisible, over-width graphemes.
        assert_eq!(
            wrap_details("界a", 1),
            vec![Line::raw("界"), Line::raw("a")]
        );
    }

    #[test]
    fn home_abbreviation_requires_an_absolute_component_prefix() {
        for (path, home, expected) in [
            ("/home/user", Some("/home/user"), "~"),
            ("/home/user/project", Some("/home/user"), "~/project"),
            ("/home/user/project", Some("/home/user/"), "~/project"),
            (
                "/home/user2/project",
                Some("/home/user"),
                "/home/user2/project",
            ),
            ("/home/user-old", Some("/home/user"), "/home/user-old"),
            ("/home/other", Some("/home/user"), "/home/other"),
            ("home/user/project", Some("home/user"), "home/user/project"),
            ("home/user/project", Some("/home/user"), "home/user/project"),
            ("/home/user", Some(""), "/home/user"),
            ("/home/user", None, "/home/user"),
            ("/", Some("/"), "~"),
            ("/project", Some("/"), "~/project"),
            ("", Some("/home/user"), ""),
        ] {
            assert_eq!(home_path(path, home), expected);
        }
    }

    #[test]
    fn statuses_have_distinct_symbols_and_labels_without_color() {
        for (status, symbol, label, color) in [
            (Status::Blocked, "!", "! Blocked", Color::Yellow),
            (Status::Error, "×", "× Error", Color::Red),
            (Status::Done, "✓", "✓ Done", Color::Green),
            (Status::Working, "●", "● Working", Color::Cyan),
            (Status::Idle, "○", "○ Idle", Color::Gray),
        ] {
            assert_eq!(status_text(status, true), symbol);
            assert_eq!(status_text(status, false), label);
            assert!(label.ends_with(status.label()));
            assert_eq!(status_color(status), color);
            assert!(!label.contains("Waiting"));
        }
    }

    #[test]
    fn compact_lines_keep_priority_and_only_clip_the_first_item() {
        assert_eq!(compact_line(&["one", "two", "three"], 9), "one · two");
        assert_eq!(compact_line(&["one", "too long", "x"], 7), "one · x");
        assert_eq!(compact_line(&["", "essential", "x"], 4), "ess…");
        assert_eq!(compact_line(&["\x1b", "界e\u{301}abc"], 4), "界e\u{301}…");
        assert_eq!(compact_line(&["abc"], 0), "");
        assert_eq!(compact_line(&[], 8), "");
        for width in 0..=160 {
            let line = compact_line(&["界e\u{301}", "👩🏽", "longer text", "x"], width);
            assert!(text_width(&line) <= usize::from(width));
        }
    }

    #[test]
    fn footer_hints_fit_all_widths_and_keep_the_requested_order() {
        for grouped in [false, true] {
            for width in 0..=160 {
                let line = shortcuts(width, grouped);
                assert!(text_width(&line) <= usize::from(width), "{width}: {line}");
                if width >= 9 {
                    assert!(line.starts_with("↑↓ Select"));
                    for part in line.split(" · ") {
                        assert!(
                            shortcuts(160, grouped)
                                .split(" · ")
                                .any(|hint| hint == part)
                        );
                    }
                }
            }
        }
        assert_eq!(shortcuts(0, true), "");
        assert_eq!(shortcuts(9, false), "↑↓ Select");
        assert_eq!(shortcuts(22, false), "↑↓ Select · Enter Jump");
        assert_eq!(
            shortcuts(160, false),
            "↑↓ Select · Enter Jump · g/G Group · / Filter · ? Help · q Quit · w Details"
        );
        assert_eq!(
            shortcuts(160, true),
            "↑↓ Select · Enter Jump · g/G Group · / Filter · ? Help · q Quit · Tab Groups · ←/→ Fold · w Details"
        );
    }
}
