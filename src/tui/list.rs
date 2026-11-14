//! Pure sorting, grouping, and filtering. Row indices refer to the supplied snapshot.

use std::cmp::Ordering;

use crate::model::{Entry, Snapshot, Status, agent_name, display_text};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum SortBy {
    #[default]
    None,
    Status,
    Agent,
    Cwd,
    Session,
}

impl SortBy {
    pub(super) fn next(self) -> Self {
        match self {
            Self::None => Self::Status,
            Self::Status => Self::Agent,
            Self::Agent => Self::Cwd,
            Self::Cwd => Self::Session,
            Self::Session => Self::None,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Status => "Status",
            Self::Agent => "Agent",
            Self::Cwd => "CWD",
            Self::Session => "Session",
        }
    }

    fn compare(self, a: &Entry, b: &Entry) -> Ordering {
        match self {
            Self::None => Ordering::Equal,
            Self::Status => (a.status, &a.location, &a.id).cmp(&(b.status, &b.location, &b.id)),
            Self::Agent => canonical_agent(a).cmp(canonical_agent(b)),
            Self::Cwd => (a.working_dir.is_none(), &a.working_dir)
                .cmp(&(b.working_dir.is_none(), &b.working_dir)),
            Self::Session => (&a.location, &a.id).cmp(&(&b.location, &b.id)),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum GroupOrder {
    #[default]
    Default,
    Reverse,
}

impl GroupOrder {
    pub(super) fn toggle(self) -> Self {
        match self {
            Self::Default => Self::Reverse,
            Self::Reverse => Self::Default,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Reverse => "Reverse",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum GroupBy {
    #[default]
    None,
    Agent,
    Status,
    Cwd,
    Session,
}

impl GroupBy {
    pub(super) fn next(self) -> Self {
        match self {
            Self::None => Self::Agent,
            Self::Agent => Self::Status,
            Self::Status => Self::Cwd,
            Self::Cwd => Self::Session,
            Self::Session => Self::None,
        }
    }

    pub(super) fn previous(self) -> Self {
        match self {
            Self::None => Self::Session,
            Self::Agent => Self::None,
            Self::Status => Self::Agent,
            Self::Cwd => Self::Status,
            Self::Session => Self::Cwd,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Agent => "Agent",
            Self::Status => "Status",
            Self::Cwd => "CWD",
            Self::Session => "Session",
        }
    }

    /// Compare group order only, independently of member sorting.
    pub(super) fn compare(self, a: &Entry, b: &Entry) -> Ordering {
        match self {
            Self::None => Ordering::Equal,
            Self::Agent => canonical_agent(a).cmp(canonical_agent(b)),
            Self::Status => a.status.cmp(&b.status),
            Self::Cwd => (a.working_dir.is_none(), &a.working_dir)
                .cmp(&(b.working_dir.is_none(), &b.working_dir)),
            Self::Session => {
                (&a.location.session, &a.id.session).cmp(&(&b.location.session, &b.id.session))
            }
        }
    }

    /// Stable sorting keeps source order for equal row keys. The caller restores
    /// source order first so None can undo any previous sorting.
    pub(super) fn sort(self, snapshot: &mut Snapshot, rows: SortBy, order: GroupOrder) {
        snapshot.entries.sort_by(|a, b| {
            let group = self.compare(a, b);
            let group = match order {
                GroupOrder::Default => group,
                GroupOrder::Reverse => group.reverse(),
            };
            group.then_with(|| rows.compare(a, b))
        });
    }

    pub(super) fn key(self, entry: &Entry) -> Option<GroupKey> {
        match self {
            Self::None => None,
            Self::Agent => Some(GroupKey::Agent(canonical_agent(entry).into())),
            Self::Status => Some(GroupKey::Status(entry.status)),
            Self::Cwd => Some(GroupKey::Cwd(entry.working_dir.clone())),
            Self::Session => Some(GroupKey::Session(entry.id.session.clone())),
        }
    }

    /// Complete single-line display data. The parent is responsible for clipping.
    pub(super) fn group_label(self, entry: &Entry) -> String {
        let label = match self {
            Self::None => "",
            Self::Agent => canonical_agent(entry),
            Self::Status => entry.status.label(),
            Self::Cwd => entry.working_dir.as_deref().unwrap_or("Unknown"),
            Self::Session => &entry.location.session,
        };
        display_text(label)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) enum GroupKey {
    Agent(String),
    Status(Status),
    Cwd(Option<String>),
    Session(String),
}

#[derive(Clone, Debug)]
pub(super) struct Group {
    pub key: GroupKey,
    pub label: String,
    pub entries: Vec<usize>,
}

/// Collect nonempty groups from entries already sorted by `mode`.
/// Filtering preserves snapshot indices and the order within each group.
pub(super) fn groups(snapshot: &Snapshot, mode: GroupBy, query: &str) -> Vec<Group> {
    if mode == GroupBy::None {
        return Vec::new();
    }
    let mut result: Vec<Group> = Vec::new();
    for index in matching_indices(snapshot, query) {
        let entry = &snapshot.entries[index];
        let Some(key) = mode.key(entry) else {
            continue;
        };
        if let Some(group) = result.last_mut().filter(|group| group.key == key) {
            group.entries.push(index);
        } else {
            result.push(Group {
                key,
                label: mode.group_label(entry),
                entries: vec![index],
            });
        }
    }
    result
}

fn canonical_agent(entry: &Entry) -> &str {
    agent_name(&entry.id.agent).unwrap_or(&entry.id.agent)
}

/// Case-insensitive substring matching. Every whitespace-separated term must match.
/// Known prefixes restrict a term to one field; unknown prefixes remain literal.
pub(super) fn matching_indices(snapshot: &Snapshot, query: &str) -> Vec<usize> {
    let query = query.to_lowercase();
    let terms: Vec<_> = query.split_whitespace().collect();
    snapshot
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| terms.iter().all(|term| matches_term(entry, term)))
        .map(|(index, _)| index)
        .collect()
}

fn matches_term(entry: &Entry, term: &str) -> bool {
    let contains = |value: &str, needle: &str| value.to_lowercase().contains(needle);
    let agent = |needle| contains(canonical_agent(entry), needle) || contains(&entry.name, needle);
    let cwd = |needle| contains(entry.working_dir.as_deref().unwrap_or("Unknown"), needle);
    let session =
        |needle| contains(&entry.location.session, needle) || contains(&entry.id.session, needle);
    let status = |needle| contains(entry.status.label(), needle);
    if let Some((prefix, value)) = term.split_once(':') {
        match prefix {
            "agent" => return agent(value),
            "cwd" => return cwd(value),
            "session" => return session(value),
            "status" => return status(value),
            _ => {}
        }
    }
    agent(term)
        || cwd(term)
        || session(term)
        || status(term)
        || contains(&entry.location.tab, term)
        || contains(&entry.location.pane, term)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EntryId, Location, Target};

    fn entry(index: usize, agent: &str, status: Status) -> Entry {
        Entry {
            id: EntryId {
                session: format!("session-{index}"),
                block: format!("pane-{index}"),
                agent: agent.into(),
            },
            target: Target {
                session: format!("session-{index}"),
                window: format!("tab-{index}"),
                block: format!("pane-{index}"),
            },
            name: agent.into(),
            status,
            location: Location {
                session: format!("Session {index}").into(),
                tab: format!("Tab {index}").into(),
                pane: format!("Pane {index}").into(),
            },
            working_dir: None,
            records: 1,
        }
    }

    fn snapshot(entries: Vec<Entry>) -> Snapshot {
        Snapshot {
            entries,
            warnings: vec!["unchanged warning".into()],
        }
    }

    fn fixture() -> Snapshot {
        snapshot(
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
                let mut entry = entry(index, agent, status);
                entry.id.session = session_id.into();
                entry.target.session = session_id.into();
                entry.location.session = session_name.into();
                entry.working_dir = cwd.map(String::from);
                entry
            })
            .collect(),
        )
    }

    #[test]
    fn modes_preserve_base_order_and_group_counts() {
        let original = fixture();
        for (mode, order, sizes) in [
            (GroupBy::None, [3, 2, 4, 1, 5, 0], vec![]),
            (GroupBy::Agent, [4, 1, 2, 5, 3, 0], vec![2, 2, 2]),
            (GroupBy::Status, [3, 2, 4, 1, 5, 0], vec![1, 1, 1, 1, 2]),
            (GroupBy::Cwd, [2, 4, 3, 0, 1, 5], vec![2, 2, 2]),
            (GroupBy::Session, [2, 1, 3, 5, 4, 0], vec![2, 2, 2]),
        ] {
            for reverse in [false, true] {
                let mut sorted = original.clone();
                if reverse {
                    sorted.entries.reverse();
                }
                mode.sort(&mut sorted, SortBy::Status, GroupOrder::Default);
                let expected: Vec<_> = order.map(|i| original.entries[i].clone()).into();
                assert_eq!(sorted.entries, expected, "{mode:?}");
                assert_eq!(sorted.warnings, original.warnings);
                let grouped = groups(&sorted, mode, "");
                assert_eq!(
                    grouped.iter().map(|g| g.entries.len()).collect::<Vec<_>>(),
                    sizes,
                    "{mode:?}"
                );
                for group in &grouped {
                    for &index in &group.entries {
                        assert_eq!(mode.key(&sorted.entries[index]), Some(group.key.clone()));
                    }
                    assert!(group.entries.windows(2).all(|pair| pair[0] < pair[1]));
                }
                mode.sort(&mut sorted, SortBy::Status, GroupOrder::Default);
                assert_eq!(sorted.entries, expected);
            }
        }
    }

    #[test]
    fn flat_sort_is_exactly_the_legacy_sort() {
        let mut expected = fixture();
        expected.sort();
        let mut actual = fixture();
        GroupBy::None.sort(&mut actual, SortBy::Status, GroupOrder::Default);
        assert_eq!(actual.entries, expected.entries);
        assert_eq!(GroupBy::None.key(&actual.entries[0]), None);
        assert_eq!(GroupBy::None.group_label(&actual.entries[0]), "");
        assert!(groups(&actual, GroupBy::None, "pi").is_empty());
    }

    #[test]
    fn legacy_location_and_id_order_is_kept_within_groups() {
        let mut a = entry(0, "pi", Status::Working);
        a.location.session = "Beta".into();
        let mut b = entry(1, "pi", Status::Working);
        b.location.session = "Alpha".into();
        let mut c = b.clone();
        c.id.block = "pane-0".into();
        let mut d = entry(3, "pi", Status::Blocked);
        d.location.session = "Zulu".into();
        let input = snapshot(vec![a, b, c, d]);
        let mut expected = input.clone();
        expected.sort();
        for mode in [
            GroupBy::Agent,
            GroupBy::Cwd,
            GroupBy::Status,
            GroupBy::Session,
        ] {
            let mut actual = input.clone();
            mode.sort(&mut actual, SortBy::Status, GroupOrder::Default);
            for group in groups(&actual, mode, "") {
                let entries: Vec<_> = group.entries.iter().map(|&i| &actual.entries[i]).collect();
                let legacy: Vec<_> = expected
                    .entries
                    .iter()
                    .filter(|e| mode.key(e).as_ref() == Some(&group.key))
                    .collect();
                assert_eq!(entries, legacy, "{mode:?}");
            }
        }
    }

    #[test]
    fn session_identity_is_exact_even_when_labels_match() {
        let mut data = fixture();
        GroupBy::Session.sort(&mut data, SortBy::Status, GroupOrder::Default);
        let grouped = groups(&data, GroupBy::Session, "");
        assert_eq!(grouped[0].label, "Alpha");
        assert_eq!(grouped[1].label, "Alpha");
        assert_eq!(grouped[0].key, GroupKey::Session("alpha-1".into()));
        assert_eq!(grouped[1].key, GroupKey::Session("alpha-2".into()));
        assert_ne!(grouped[0].key, grouped[1].key);
        assert_eq!(groups(&data, GroupBy::Session, "session:alpha-2").len(), 1);
    }

    #[test]
    fn agent_groups_use_canonical_ids_not_display_names() {
        let mut a = entry(0, "claude", Status::Idle);
        a.name = "Shared name".into();
        let mut b = entry(1, "claude-code", Status::Blocked);
        b.name = "Other name".into();
        let mut c = entry(2, "pi", Status::Working);
        c.name = a.name.clone();
        assert_eq!(GroupBy::Agent.compare(&a, &b), Ordering::Equal);
        assert_ne!(GroupBy::Agent.compare(&a, &c), Ordering::Equal);
        let mut data = snapshot(vec![a, b, c]);
        GroupBy::Agent.sort(&mut data, SortBy::Status, GroupOrder::Default);
        let grouped = groups(&data, GroupBy::Agent, "");
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].key, GroupKey::Agent("claude-code".into()));
        assert_eq!(grouped[0].label, "claude-code");
        assert_eq!(grouped[0].entries.len(), 2);
    }

    #[test]
    fn unknown_directories_form_one_last_group() {
        let mut data = fixture();
        let mut literal = entry(6, "pi", Status::Working);
        literal.working_dir = Some("Unknown".into());
        data.entries.push(literal);
        GroupBy::Cwd.sort(&mut data, SortBy::Status, GroupOrder::Default);
        let grouped = groups(&data, GroupBy::Cwd, "");
        assert_eq!(grouped.len(), 4);
        assert_eq!(grouped[2].key, GroupKey::Cwd(Some("Unknown".into())));
        let unknown = grouped.last().unwrap();
        assert_eq!(unknown.key, GroupKey::Cwd(None));
        assert_eq!(unknown.label, "Unknown");
        assert_eq!(unknown.entries.len(), 2);
        assert!(
            unknown
                .entries
                .iter()
                .all(|&i| data.entries[i].working_dir.is_none())
        );
    }

    #[test]
    fn labels_are_sanitized_without_path_abbreviation_or_length_limit() {
        let mut a = entry(0, "pi", Status::Idle);
        let path = format!("/root/{}/project", "long/".repeat(100));
        a.working_dir = Some(format!("{path}\n\u{202e}"));
        a.location.session = "Session\n\u{202e}name".into();
        assert_eq!(GroupBy::Cwd.group_label(&a), path);
        assert_eq!(GroupBy::Session.group_label(&a), "Sessionname");
        assert_eq!(GroupBy::Status.group_label(&a), "Idle");
        let mut b = a.clone();
        b.working_dir = Some(format!("/other/{}/project", "long/".repeat(100)));
        assert_ne!(GroupBy::Cwd.key(&a), GroupBy::Cwd.key(&b));
    }

    fn filter_fixture() -> Snapshot {
        let mut a = entry(0, "claude-code", Status::Working);
        a.name = "Assistant Écho".into();
        a.working_dir = Some("/Work/Project".into());
        a.location.session = "Production".into();
        a.location.tab = "Build Tools".into();
        a.location.pane = "Review Pane".into();
        let mut b = entry(1, "pi", Status::Idle);
        b.name = "Status:Future".into();
        snapshot(vec![a, b])
    }

    #[test]
    fn general_terms_match_each_field_without_case_sensitivity() {
        let data = filter_fixture();
        for query in [
            "CLAUDE-CODE",
            "ASSISTANT",
            "ÉCHO",
            "working",
            "/WORK/PROJECT",
            "production",
            "session-0",
            "BUILD",
            "review",
        ] {
            assert_eq!(matching_indices(&data, query), vec![0], "{query}");
        }
        assert_eq!(matching_indices(&data, "UNKNOWN"), vec![1]);
        assert_eq!(matching_indices(&data, "  \t\n"), vec![0, 1]);
    }

    #[test]
    fn known_prefixes_restrict_the_field() {
        let data = filter_fixture();
        for query in [
            "AGENT:CLAUDE",
            "agent:ÉCHO",
            "cwd:/work/project",
            "session:PRODUCTION",
            "session:session-0",
            "status:WORKING",
        ] {
            assert_eq!(matching_indices(&data, query), vec![0], "{query}");
        }
        for query in [
            "agent:working",
            "cwd:production",
            "session:review",
            "status:claude",
            "agent:project",
            "cwd:build",
            "status:future",
        ] {
            assert!(matching_indices(&data, query).is_empty(), "{query}");
        }
        assert_eq!(matching_indices(&data, "cwd:unknown"), vec![1]);
        assert_eq!(matching_indices(&data, "status:idle"), vec![1]);
        assert!(matching_indices(&data, "status:unknown").is_empty());
    }

    #[test]
    fn terms_are_anded_and_unknown_prefixes_are_literal() {
        let mut data = filter_fixture();
        data.entries[0].location.tab = "branch:Feature".into();
        for query in [
            "  agent:CLAUDE\tstatus:WORKING\ncwd:PROJECT session:Production review ",
            "branch:FEATURE assistant",
        ] {
            assert_eq!(matching_indices(&data, query), vec![0]);
        }
        assert!(matching_indices(&data, "assistant unknown").is_empty());
        assert!(matching_indices(&data, "other:assistant").is_empty());
        // Status filters match labels; they do not apply the schema's Idle fallback.
        assert!(matching_indices(&data, "status:sleeping").is_empty());
        // Literal substring matching, without regex or fuzzy expansion.
        assert!(matching_indices(&data, "agent:cl.*").is_empty());
        assert!(matching_indices(&data, "assitant").is_empty());
    }

    #[test]
    fn filtering_keeps_original_indices_and_removes_empty_groups() {
        let mut data = fixture();
        GroupBy::Agent.sort(&mut data, SortBy::Status, GroupOrder::Default);
        assert_eq!(matching_indices(&data, "agent:codex status:idle"), vec![3]);
        let grouped = groups(&data, GroupBy::Agent, "agent:codex status:idle");
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].key, GroupKey::Agent("codex".into()));
        assert_eq!(grouped[0].entries, vec![3]);
        let filtered = groups(&data, GroupBy::Agent, "cwd:/work");
        assert_eq!(filtered.len(), 3);
        assert_eq!(filtered[0].entries, vec![0]);
        assert_eq!(filtered[1].entries, vec![2]);
        assert_eq!(filtered[2].entries, vec![4, 5]);
        assert!(groups(&data, GroupBy::Agent, "no-match").is_empty());
    }

    #[test]
    fn row_sort_modes_use_their_own_keys_and_preserve_equal_source_order() {
        let original = fixture();
        for (mode, order) in [
            (SortBy::None, [0, 1, 2, 3, 4, 5]),
            (SortBy::Status, [3, 2, 4, 1, 5, 0]),
            (SortBy::Agent, [1, 4, 2, 5, 0, 3]),
            (SortBy::Cwd, [2, 4, 0, 3, 1, 5]),
            (SortBy::Session, [1, 2, 3, 5, 0, 4]),
        ] {
            let mut actual = original.clone();
            GroupBy::None.sort(&mut actual, mode, GroupOrder::Default);
            let expected: Vec<_> = order.map(|index| original.entries[index].clone()).into();
            assert_eq!(actual.entries, expected, "{mode:?}");
            assert_eq!(actual.warnings, original.warnings);
            GroupBy::None.sort(&mut actual, mode, GroupOrder::Default);
            assert_eq!(actual.entries, expected, "{mode:?} is not stable");
        }
    }

    #[test]
    fn reversing_groups_never_reverses_members_for_any_sort_mode() {
        for mode in [
            GroupBy::Agent,
            GroupBy::Status,
            GroupBy::Cwd,
            GroupBy::Session,
        ] {
            for rows in [
                SortBy::None,
                SortBy::Status,
                SortBy::Agent,
                SortBy::Cwd,
                SortBy::Session,
            ] {
                let mut normal = fixture();
                let mut reverse = normal.clone();
                mode.sort(&mut normal, rows, GroupOrder::Default);
                mode.sort(&mut reverse, rows, GroupOrder::Reverse);
                let expected: Vec<_> = groups(&normal, mode, "")
                    .into_iter()
                    .rev()
                    .flat_map(|group| {
                        group
                            .entries
                            .into_iter()
                            .map(|index| normal.entries[index].clone())
                    })
                    .collect();
                assert_eq!(reverse.entries, expected, "{mode:?}, {rows:?}");
            }
        }
    }

    #[test]
    fn agent_sort_uses_canonical_identity_and_cwd_sort_uses_full_paths() {
        let mut a = entry(0, "claude", Status::Idle);
        a.name = "zzz".into();
        a.working_dir = Some("/work/b/project".into());
        let mut b = entry(1, "claude-code", Status::Blocked);
        b.name = "aaa".into();
        b.working_dir = Some("/work/a/project".into());
        let missing = entry(2, "codex", Status::Working);
        let original = snapshot(vec![a.clone(), missing.clone(), b.clone()]);
        let mut actual = original.clone();
        GroupBy::None.sort(&mut actual, SortBy::Agent, GroupOrder::Default);
        assert_eq!(actual.entries, vec![a.clone(), b.clone(), missing.clone()]);
        actual = original;
        GroupBy::None.sort(&mut actual, SortBy::Cwd, GroupOrder::Default);
        assert_eq!(actual.entries, vec![b, a, missing]);
    }

    #[test]
    fn row_and_group_order_cycles_have_independent_defaults() {
        assert_eq!(SortBy::default(), SortBy::None);
        let mut rows = SortBy::None;
        for (expected, label) in [
            (SortBy::Status, "Status"),
            (SortBy::Agent, "Agent"),
            (SortBy::Cwd, "CWD"),
            (SortBy::Session, "Session"),
            (SortBy::None, "None"),
        ] {
            rows = rows.next();
            assert_eq!(rows, expected);
            assert_eq!(rows.label(), label);
        }
        let order = GroupOrder::default();
        assert_eq!(order, GroupOrder::Default);
        assert_eq!(order.label(), "Default");
        assert_eq!(order.toggle(), GroupOrder::Reverse);
        assert_eq!(order.toggle().label(), "Reverse");
        assert_eq!(order.toggle().toggle(), order);
    }

    #[test]
    fn empty_snapshots_have_no_indices_or_groups() {
        let data = Snapshot::default();
        assert!(matching_indices(&data, "").is_empty());
        for mode in [
            GroupBy::None,
            GroupBy::Agent,
            GroupBy::Status,
            GroupBy::Cwd,
            GroupBy::Session,
        ] {
            assert!(groups(&data, mode, "").is_empty());
        }
    }

    #[test]
    fn forward_and_reverse_cycles_are_inverses() {
        assert_eq!(GroupBy::default(), GroupBy::None);
        let mut mode = GroupBy::None;
        for (expected, label) in [
            (GroupBy::Session, "Session"),
            (GroupBy::Cwd, "CWD"),
            (GroupBy::Status, "Status"),
            (GroupBy::Agent, "Agent"),
            (GroupBy::None, "None"),
        ] {
            assert_eq!(mode.next().previous(), mode);
            assert_eq!(mode.previous().next(), mode);
            mode = mode.previous();
            assert_eq!(mode, expected);
            assert_eq!(mode.label(), label);
        }
        for expected in [
            GroupBy::Agent,
            GroupBy::Status,
            GroupBy::Cwd,
            GroupBy::Session,
            GroupBy::None,
        ] {
            mode = mode.next();
            assert_eq!(mode, expected);
        }
    }
}
