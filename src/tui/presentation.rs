//! Pure text and layout helpers. Display values do not change model data.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
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

fn text_width(value: &str) -> usize {
    Line::raw(value).width()
}

/// Wrap words in terminal cells and preserve all spaces. Split long words only
/// between graphemes. Joining the lines restores the exact input.
pub(super) fn wrap_words(value: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    for (index, word) in value.split(' ').enumerate() {
        for part in [if index == 0 { "" } else { " " }, word] {
            let size = text_width(part);
            if used + size > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            for grapheme in Span::raw(part).styled_graphemes(Style::default()) {
                let size = text_width(grapheme.symbol);
                if used + size > width && !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                    used = 0;
                }
                line.push_str(grapheme.symbol);
                used += size;
            }
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// An indented label/value row with a hanging indent for wrapped values.
/// Narrow views put the label on its own line instead of squeezing the value.
pub(super) fn field_lines(
    label: &str,
    value: &str,
    label_width: usize,
    width: u16,
    label_style: Style,
    value_style: Style,
) -> Vec<Line<'static>> {
    let width = usize::from(width);
    let label = display_text(label);
    let value = display_text(value);
    let indent = if width >= 8 { 2 } else { 0 };
    // Unlabeled rows align with the indent; labeled rows add a two-cell gap.
    let column = indent + if label_width == 0 { 0 } else { label_width + 2 };
    if width >= column + 12 {
        return wrap_words(&value, width - column)
            .into_iter()
            .enumerate()
            .map(|(index, part)| {
                let lead = if index == 0 {
                    label.clone()
                } else {
                    String::new()
                };
                let pad = (column - indent).saturating_sub(text_width(&lead));
                Line::from(vec![
                    Span::raw(" ".repeat(indent)),
                    Span::styled(lead, label_style),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(part, value_style),
                ])
            })
            .collect();
    }
    let mut lines = Vec::new();
    if !label.is_empty() {
        for part in wrap_words(&label, width.saturating_sub(indent)) {
            lines.push(Line::from(vec![
                Span::raw(" ".repeat(indent)),
                Span::styled(part, label_style),
            ]));
        }
    }
    let nested = if width >= 12 { indent + 2 } else { indent };
    for part in wrap_words(&value, width.saturating_sub(nested)) {
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(nested)),
            Span::styled(part, value_style),
        ]));
    }
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

/// The heading for a data column. A clipped "Working directory" becomes "CWD".
pub(super) fn column_label(index: usize, width: u16) -> &'static str {
    let label = COLUMN_LABELS[index];
    if index == 2 && text_width(label) > usize::from(width) && width >= 3 {
        "CWD"
    } else {
        label
    }
}

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

/// Status foreground for a row. Status colors are hard to read on the selection
/// background, so the selected row uses white and the symbol identifies the status.
pub(super) fn status_fg(status: Status, selected: bool) -> Color {
    if selected {
        Color::White
    } else {
        status_color(status)
    }
}

pub(super) const STATUSES: [Status; 5] = [
    Status::Blocked,
    Status::Error,
    Status::Done,
    Status::Working,
    Status::Idle,
];

/// Nonzero per-status counts in priority order, such as `!1 ●2`.
pub(super) fn status_summary(
    statuses: impl IntoIterator<Item = Status>,
    selected: bool,
) -> Vec<Span<'static>> {
    let mut counts = [0usize; 5];
    for status in statuses {
        counts[status as usize] += 1;
    }
    let mut spans = Vec::new();
    for (status, count) in STATUSES.into_iter().zip(counts) {
        if count == 0 {
            continue;
        }
        if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            format!("{}{count}", status_text(status, true)),
            Style::default().fg(status_fg(status, selected)),
        ));
    }
    spans
}

pub(super) fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(Span::width).sum()
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

/// Styled form of [`compact_line`]: skip unfit parts and only clip the first one.
pub(super) fn compact_spans(parts: &[Span<'static>], width: u16) -> Line<'static> {
    let limit = usize::from(width);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for part in parts {
        let content = display_text(&part.content);
        if content.is_empty() {
            continue;
        }
        let size = text_width(&content);
        if spans.is_empty() {
            if size > limit {
                return Line::from(Span::styled(clip_text(&content, width, false), part.style));
            }
            spans.push(Span::styled(content, part.style));
            used = size;
        } else if used + 3 + size <= limit {
            spans.push(Span::raw(" · "));
            spans.push(Span::styled(content, part.style));
            used += 3 + size;
        }
    }
    Line::from(spans)
}

pub(super) fn plural(count: usize, singular: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { plural })
}

/// Footer hints. A nonzero attention count advertises the `a` jump.
pub(super) fn shortcuts(width: u16, grouped: bool, attention: usize) -> Line<'static> {
    let mut parts = vec![Span::raw("↑↓ Select"), Span::raw("Enter Jump")];
    if attention > 0 {
        parts.push(Span::styled(
            format!("a Attention ({attention})"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    parts.extend([
        Span::raw("/ Filter"),
        Span::raw("g/G Group"),
        Span::raw("? Help"),
        Span::raw("q Quit"),
    ]);
    if grouped {
        parts.extend([Span::raw("Tab Groups"), Span::raw("←/→ Fold")]);
    }
    parts.push(Span::raw("w Details"));
    compact_spans(&parts, width)
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
    fn word_wrap_keeps_words_and_graphemes_and_never_loses_text() {
        assert_eq!(wrap_words("one two three", 7), vec!["one two", " three"]);
        assert_eq!(wrap_words("a abcdefg", 7), vec!["a ", "abcdefg"]);
        assert_eq!(wrap_words("界e\u{301}abc", 3), vec!["界e\u{301}", "abc"]);
        assert_eq!(wrap_words("", 5), vec![""]);
        assert_eq!(wrap_words("界a", 1), vec!["界", "a"]);
        assert_eq!(wrap_words("a  b", 0), vec!["a", " ", " ", "b"]);
        let value = "session:12345678-1234 界e\u{301}/👩🏽/project tail";
        for width in 1..=40 {
            let lines = wrap_words(value, width);
            for line in &lines {
                // A single grapheme wider than the line is the only allowed overflow.
                assert!(text_width(line) <= width.max(2), "{width}: {line:?}");
            }
            assert_eq!(lines.concat(), value, "{width}");
        }
    }

    #[test]
    fn word_wrap_preserves_significant_spaces() {
        for value in [
            "",
            " ",
            "  leading",
            "trailing  ",
            "/work/my  project",
            "  界e\u{301}/👩🏽  tail  ",
        ] {
            for width in 0..=60 {
                let lines = wrap_words(value, width);
                assert_eq!(lines.concat(), value, "{width}: {lines:?}");
                assert!(lines.iter().all(|line| text_width(line) <= width.max(2)));
                let fields = field_lines(
                    "",
                    value,
                    0,
                    width as u16,
                    Style::default(),
                    Style::default(),
                );
                let text: String = fields
                    .iter()
                    .map(|line| line.spans.last().unwrap().content.as_ref())
                    .collect();
                assert_eq!(text, value, "{width}: {fields:?}");
            }
        }
    }

    #[test]
    fn fields_align_values_and_fall_back_to_stacked_labels_when_narrow() {
        let plain = Style::default();
        let text = |lines: Vec<Line>| lines.iter().map(Line::to_string).collect::<Vec<_>>();
        assert_eq!(
            text(field_lines("Agent", "pi", 8, 40, plain, plain)),
            vec!["  Agent     pi"]
        );
        assert_eq!(
            text(field_lines(
                "Tab",
                "one two three four",
                5,
                21,
                plain,
                plain
            )),
            vec!["  Tab    one two ", "         three four"]
        );
        assert_eq!(
            text(field_lines("", "note text", 0, 30, plain, plain)),
            vec!["  note text"]
        );
        assert_eq!(
            text(field_lines(
                "Working directory",
                "/a/b",
                17,
                20,
                plain,
                plain
            )),
            vec!["  Working directory", "    /a/b"]
        );
        for width in 0..=60 {
            for line in field_lines(
                "Label",
                "界e\u{301} value with words",
                9,
                width,
                plain,
                plain,
            ) {
                assert!(line.width() <= usize::from(width).max(2), "{width}: {line}");
            }
        }
    }

    #[test]
    fn clipped_working_directory_heading_uses_cwd() {
        assert_eq!(column_label(2, 17), "Working directory");
        assert_eq!(column_label(2, 16), "CWD");
        assert_eq!(column_label(2, 3), "CWD");
        assert_eq!(column_label(2, 2), "Working directory");
        assert_eq!(column_label(1, 3), "Agent");
    }

    #[test]
    fn status_summaries_count_nonzero_statuses_in_priority_order() {
        let summary = status_summary(
            [Status::Idle, Status::Blocked, Status::Idle, Status::Working],
            false,
        );
        assert_eq!(Line::from(summary.clone()).to_string(), "!1 ●1 ○2");
        assert_eq!(spans_width(&summary), 8);
        assert_eq!(summary[0].style.fg, Some(Color::Yellow));
        assert_eq!(summary[4].style.fg, Some(Color::Gray));
        let selected = status_summary([Status::Idle, Status::Blocked, Status::Done], true);
        assert!(
            selected
                .iter()
                .filter(|span| span.content != " ")
                .all(|span| span.style.fg == Some(Color::White))
        );
        assert!(status_summary([], false).is_empty());
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
            for attention in [0, 3] {
                let full = shortcuts(200, grouped, attention).to_string();
                for width in 0..=160 {
                    let line = shortcuts(width, grouped, attention).to_string();
                    assert!(text_width(&line) <= usize::from(width), "{width}: {line}");
                    if width >= 9 {
                        assert!(line.starts_with("↑↓ Select"));
                        for part in line.split(" · ") {
                            assert!(full.split(" · ").any(|hint| hint == part));
                        }
                    }
                }
            }
        }
        assert_eq!(shortcuts(0, true, 0).to_string(), "");
        assert_eq!(shortcuts(9, false, 0).to_string(), "↑↓ Select");
        assert_eq!(
            shortcuts(22, false, 0).to_string(),
            "↑↓ Select · Enter Jump"
        );
        assert_eq!(
            shortcuts(160, false, 0).to_string(),
            "↑↓ Select · Enter Jump · / Filter · g/G Group · ? Help · q Quit · w Details"
        );
        assert_eq!(
            shortcuts(160, true, 0).to_string(),
            "↑↓ Select · Enter Jump · / Filter · g/G Group · ? Help · q Quit · Tab Groups · ←/→ Fold · w Details"
        );
        // The attention jump is advertised third and still fits a 40-cell view.
        let line = shortcuts(40, false, 3);
        assert_eq!(line.to_string(), "↑↓ Select · Enter Jump · a Attention (3)");
        let hint = line
            .spans
            .iter()
            .find(|span| span.content.starts_with("a "))
            .unwrap();
        assert_eq!(hint.style.fg, Some(Color::Yellow));
    }

    #[test]
    fn styled_compact_lines_match_plain_compact_lines() {
        for width in 0..=40 {
            let parts = ["界e\u{301}", "👩🏽", "longer text", "x"];
            let spans: Vec<_> = parts.iter().map(|part| Span::raw(*part)).collect();
            assert_eq!(
                compact_spans(&spans, width).to_string(),
                compact_line(&parts, width)
            );
        }
        assert_eq!(plural(1, "warning", "warnings"), "1 warning");
        assert_eq!(plural(2, "warning", "warnings"), "2 warnings");
    }
}
