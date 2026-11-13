# Rex compatibility and verification handoff

## Measured environment

The implementation capability check used:

| Component | Observed value | Source |
| --- | --- | --- |
| Rust | `rustc 1.99.1 (58cf8f9c3 2026-10-13)` | rustup stable, `~/.cargo/bin/rustc` |
| Cargo | `cargo 1.99.1 (5f94df478 2026-08-27)` | rustup stable, `~/.cargo/bin/cargo` |
| Rex CLI/server | `0.0.1793981048-g01d6c4`, commit `01d6c4fe68668db3521570d7fbc9b6b112b1e572` | App-managed `RexServer/Versions/Current/Rex Server.app/Contents/MacOS/rex` |
| Server API | Revision 11 | `rex version` and `rex whoami --json` |
| App | Rex Beta `0.1.0 (1853)`, macOS | Connected client metadata and app bundle |

The supplied plan had prior evidence for preview.14 / API 12 / app build 1925. Those versions were **not** available here and are not claimed as tested. Rex uses `rex version`; `rex --version` is not accepted on the measured installation.

No repository-specific `AGENTS.md` or contribution policy was present. The repository started with README, MIT license, and Rust ignore rules. The four standard checks from the supplied plan are the required gates.

## Repeatable read-only capability checks

Use IDs from the current installation. Do not copy IDs, endpoints, host names, or session paths from another computer.

```sh
rustc --version
cargo --version
command -v rex
rex version
rex --help
rex --autostart=false whoami --json
rex --autostart=false ls --json
rex --autostart=false session inspect "$REX_SESSION" --json
rex --autostart=false block ls -s "$REX_SESSION" --json
rex --autostart=false block inspect "$REX_BLOCK" --json
rex --autostart=false block call -s "$REX_SESSION" -b "$REX_BLOCK" program_status
rex --autostart=false block call -s "$REX_SESSION" -b "$REX_BLOCK" process
rex --autostart=false client ls --json
rex --autostart=false api describe session.focus_block --json
rex --autostart=false api describe session.view --json
rex --autostart=false api describe session.list_blocks --json
rex --autostart=false api describe client.perform --json
rex --autostart=false -C "$CLIENT_ID" actions --json
rex events --help
```

`CLIENT_ID` must be the full ID of the intended connected app. Action discovery does not execute an action or prove Remote Control permission.

A bounded observation of `rex --autostart=false events --all --json` was made with a subprocess deadline and cleanup. It delivered newline-delimited server event objects, including `client_joined` and `client_left`. No app switch, pane creation, input injection, permission change, or user-session rename was performed.

## De-identified observations

The live snapshot had the following structure (identifiers replaced):

```json
{
  "session_id": "session:<uuid>",
  "active_window_id": "window:<uuid>",
  "windows": [{
    "window_id": "window:<uuid>",
    "focused_block_id": "block:<uuid>",
    "layers": [{
      "blocks": [{
        "block_id": "block:<uuid>",
        "creator_name": "com.superlogical.terminal"
      }]
    }]
  }]
}
```

The block list supplied `placement: "placed"` and a matching `window_id`. The program status was:

```json
{"records":[{"state":"working","app":"pi","effective_app":"pi","owner":{"pid":123,"name":"node"}}]}
```

Foreground process metadata included `name: "node"` and `argv0: "pi"`. Paths, user names, real process IDs, timestamps, and endpoint values are not retained.

The installed block schema exposes `program_status`, `process`, `program_status_changed`, and `program_status_removed`. The server focus schema requires session and block IDs. The client action schema exposes `session.select` with required `session_id` and optional `window_id`.

The older status schema suggests mapping unknown states to idle. Brachio deliberately maps them to **Unknown**, as required by the plan. The TUI changes do not change the status model: Blocked, Error, Done, Unknown, Working, and Idle. Unknown is neutral gray, never inferred Working or Idle. Blocked does not establish that input is needed; no Waiting state is inferred. Done/Error can survive process exit. Done is only a reported state, not whole-session completion. A report timestamp is not task duration.

The documented raw terminal-event envelope is:

```json
{"type":"session_event","payload":{"session_id":"session:<uuid>","event":{"type":"block_event","payload":{"block_id":"block:<uuid>","creator_name":"com.superlogical.terminal","name":"program_status_changed","payload":{"id":"","record":{"app":"pi","state":"working"},"reason":"report"}}}}}
```

Only the envelope type is needed: events trigger a new authoritative snapshot, not incremental record edits. CLI client join/leave churn is ignored to prevent snapshot feedback loops. A fixed reconciliation also covers missed events and client changes.

The working-directory column uses `foreground.cwd` only when the foreground process has the same recognized agent identity as the row. It does not use a shell/other program directory for retained agent records. Missing, relative, NUL-containing, or oversized paths remain unknown. Session/tab/pane labels are independent display fields; exact IDs remain navigation data and are available in Details.

The responsive list keeps Status, Agent, CWD, Session, Tab, and Pane independent. Widths measure sanitized terminal-cell content and headings across all matching agents, including collapsed members, with two-cell gaps and bounded natural widths. Extra space stays on the right; selection highlighting spans the full row. Values shorten before the fit calculation hides Pane, Tab, and Session in priority order. Very small views use status symbols and shorter essential values. Visibility is content-driven rather than based on fixed width breakpoints.

Group headers with matching counts appear only for at least two nonempty filtered groups. Under actual multiple headers, grouping hides only the repeated Status, Agent, CWD, or Session column, with no leftover gap. Single-group results restore that column; Tab and Pane are never merged. `HOME` abbreviation to `~` requires an absolute path-component prefix and changes display only. Details keeps original full paths, independent location values, IDs, status source, messages, and warnings. The width-bounded footer uses compact connection/count/attention/working text, skips zero warnings, and directs diagnostics to Details instead of showing UUIDs or full errors.

The server describes session focus as shared session state. A client action can be refused, disconnected, unsupported, or timed out. A timed-out action can still execute later. There is no verified atomic operation that both focuses a block and switches only one app. These limits are documented rather than hidden.

## Automated verification scope

- Model tests: multiple apps, related records, parent inheritance, missing/future states, clear, unsupported apps, safe text, and exact resource IDs. Location labels stay separate from IDs; directory changes cannot change agent identity or navigation targets.
- Mock CLI tests: multiple sessions/tabs/panes, detached and nonterminal blocks, empty status, retained completed reports, malformed JSON, unsupported status, ambiguous locations, pinned app choice, transient startup discovery recovery, refusal to replace a pin, precise ordered arguments, stale/moved panes, focus failure, permission refusal, event refresh, reconnect, navigation during a slow scan, and worker shutdown.
- TUI tests: empty and long lists, the default original ungrouped sort, Agent/Status/CWD/Session grouping, foldable count headers only for multiple filtered groups, toggle-only header clicks, header-aware scroll, stable selected IDs through folds and filtering, expansion-only Enter on a collapsed selection, forward/reverse mode cycles, next/previous group selection and wraparound, canonical agent and full-path group keys, unknown CWD grouping, and same-name session separation. Coverage also includes incremental case-insensitive AND filters and field prefixes, filter apply/cancel/clear, hidden-selection navigation blocking, Blocked/Error attention jumps, responsive columns, HOME display abbreviation, status symbols/colors, Unicode clipping, bounded footer text, tiny viewports, scroll, resize, removed targets, stale click maps, keyboard/mouse target equivalence, disabled navigation, and complete scrollable Details/Help. Input tests confirm that clicks before or after a Details redraw do not replace error messages.
- Subprocess tests: timeout, cancellation, argument boundaries, invalid encoding, output limits, stderr failures, stream framing, EOF, and owned descendant cleanup.
- PTY smoke tests: actual binary startup and keyboard/signal exits with terminal-mode and escape cleanup, including bracketed-paste setup/cleanup, multiline filter paste kept as text, and SIGTERM during sustained input. Nonterminal and argument errors exit without starting the TUI.

### Local results

All four standard checks passed on macOS with the measured Rust toolchain. The suite has 190 tests: 156 library tests, two redraw/input routing tests, 19 mock Rex integration tests, and 13 PTY/CLI smoke tests. Three additional complete test runs passed before the location/working-directory update. Release binary help/version checks also passed without terminal setup or Rex navigation.

Repeated verification exposed and corrected a Darwin process-exit transition where group signaling can report EPERM before waitid exposes exit. The runner retains child identity during a bounded retry; a 100-short-stream regression checks this path. Process and mock CLI fixtures use serialized execution to avoid cold-executable launch saturation. Worker concurrency remains active within each fixture, including the slow-scan navigation test. Process fixtures also use startup-specific test deadlines; production request deadlines remain unchanged.

Source review also corrected queued-click retargeting, duplicate-parent record inheritance, input-flood shutdown, and final terminal cleanup after a worker panic. Automated PR review identified non-mutating mouse events that consumed click targets, hidden warning details, startup discovery that did not recover, and navigation delayed by snapshot scans. The fixes preserve motion/release hit targets, expose scrollable details, retry only transient pre-pin discovery, and separate serialized navigation from read-only refresh work. No live app switch or agent input was sent during verification. The CI workflow repeats the standard checks on Linux and macOS; native UI acceptance remains a separate handoff.

During grouping verification, `process::tests::timeout_kills_and_reaps_the_group_with_inherited_pipes` once failed because a fixture PID file was absent. Its isolated retry and the complete 126-test suite passed without changes to process code.

The TUI UX update was checked with synthetic grouped layouts and a sixty-agent filter/group/resize/refresh fixture. The compact-column update checks every width from zero through 200 cells, all 64 hidden-field masks, and offset bounds. Integrated checks cover 120×30, 100×25, 80×24, 60×20, and 40×15 terminals, independent field alignment, stable widths through folds/scroll, Unicode clipping, full-row selection highlighting, and exact keyboard/mouse navigation. Twenty synthetic None/Agent/CWD/Session layouts were reviewed. Review found and corrected missing bracketed-paste setup and a bounded-arrow case that replaced a collapsed group's retained member. Owned-PTY and exact-identity regressions cover both fixes. Test-backend output does not establish native font, color, input-delivery, or app-display acceptance.

These tests are not evidence that a real Rex app displayed the target pane.

## External handoff

### Rex app user: permissions and environment

Prerequisite: install the official Rex CLI and app, connect to the intended server, and run the read-only checks above.

Decide whether to enable Remote Control in the intended app. Brachio must not enable or bypass it. Without permission, expect an explicit app-selection error; server focus can already have changed.

Unverified: other Rex versions, other agents' actual reports, and Remote Control behavior on other installations. Do not advertise them as tested platforms or agent integrations.

### Native UI tester: end-to-end behavior

Prerequisite: a prepared Rex app with multiple sessions, tabs, panes, recognized reporting agents, and permission granted by its user. Use a disposable test session if shared focus could disturb other clients.

1. Start Brachio in a pane or tab. Confirm the expected agents, statuses, and locations.
2. Use Enter and left click on the same entry. Confirm that the intended app shows exactly its session, tab, and pane.
3. Return to the TUI and repeat after status changes, scrolling, and terminal resize.
   - Use multiple agent types, statuses, directories, and sessions.
   - Use at least 50 agents. Check wide, narrow, and tiny terminal sizes, repeated resize, scrolling, and long Unicode labels/paths. Confirm that selection keeps its exact ID and remains visible after keyboard selection or resize.
   - Confirm that None is the default and keeps the original status/location/exact-identity sort, without group headers or a grouping title. Confirm that a solid line appears below column headings when space permits and keyboard hints appear only below the list.
   - Press `g` and `G` (Shift+g) through None, Agent, Status, CWD, and Session in both directions. Confirm that the selected ID stays the same. In each grouped mode, confirm that foldable headers and matching counts appear only for two or more nonempty filtered groups. Zero or one group must have no headers or folds.
   - Press `Tab` / `Shift+Tab` to select the first matching agent of the next / previous group, not the next / previous agent. Check wraparound, no visible selection, one group, and no effect in None. Confirm that jumps do not navigate or expand collapsed groups.
   - Use Left / Right to collapse / expand the selected group in every grouped mode. Confirm that collapsing retains the selected ID. Enter must expand only; a second Enter must navigate to that exact ID. Click another group header and confirm that only its fold changes, with no selection change or navigation. Column-heading clicks must do nothing.
   - Use Up / Down and k / j to select agents; expanded headers must be skipped and collapsed headers must be selectable. Use `a` / `A` for next / previous matching Blocked/Error agents, including wraparound and automatic group expansion. None of these selection keys may navigate Rex.
   - Press `/` and check incremental case-insensitive AND matching, including `agent:`, `cwd:`, `session:`, and `status:` terms. Test typing, paste, Backspace, zero matches, and filters that leave one group. Enter must apply without navigation; Escape must restore the previous query during editing. On the list, Escape must clear a query before it can exit. A filtered-out selected ID must not navigate or silently transfer selection.
   - Check 120×30, 100×25, 80×24, 60×20, and 40×15 terminals. Status, Agent, CWD, Session, Tab, and Pane must remain independent. Short values must stay compact and left-aligned on wide terminals, with two spaces between columns and a full-width selected-row highlight. Confirm that narrowing shortens values before hiding lower-priority fields and never overlaps text.
   - Under multiple headers only, Status/Agent/CWD/Session grouping must remove its matching column without leaving a gap. Tab and Pane must stay separate. With one group, the grouped column must return when space permits. Confirm that folding and scrolling do not change measured widths; check CJK labels and long path tails. Confirm display-only `HOME` abbreviation at path-component boundaries and unchanged full paths and IDs in Details.
   - Check status glyphs, Unicode clipping, and contrast with the tester's fonts and light/dark terminal colors. Unknown must remain neutral; selected Idle labels and compact symbols must contrast with the selection background. Done must not imply session completion. Confirm distinct status symbols without color and no inferred Waiting state.
   - Confirm that the footer fits the terminal width, omits zero warnings and UUIDs/full diagnostics, and shows compact counts, attention, and working values when space permits. Open `w` for full diagnostics, status source, paths, and IDs; open `?` for complete help. Scroll each overlay with arrows, page keys, and wheel. Group/filter/fold keys must not change the list while an overlay is open.
   - Paste multiline text containing q, g, and Enter into the filter. Confirm that it remains filter text rather than executing list commands, and that bracketed-paste mode is restored on exit.
   - Confirm that q / Escape closes Details or Help first, while Ctrl-C quits globally, including during filter editing. Check physical delivery of g/G, Tab/Shift+Tab, Left/Right, /, Escape, Enter, and mouse clicks. Repeat exact-ID Enter/click navigation in every mode after filters, folds, status changes, scroll, and resize.
4. Close the selected pane. Confirm that the entry disappears and Enter cannot silently select another row.
5. Disconnect/reconnect Rex. Confirm stale indication, navigation blocking, and eventual resynchronization.
6. Test with Remote Control disabled and with multiple app clients. Confirm refusal or explicit client selection, without fallback.
7. Observe another client attached to the same test session and document shared focus effects.
8. Exit Brachio. Confirm that normal terminal input, mouse behavior, screen, and cursor return.

Expected result: only the requested navigation occurs; no prompt, key, agent command, pane closure, or permission change is sent.

Unverified: physical group/filter/fold-key and mouse delivery, font/color rendering, visible app switching, return-to-TUI behavior, 50+ agent native resize behavior, and shared-client UI effects. **Do not claim native end-to-end platform acceptance before these checks pass.**

### Package maintainer: release

Prerequisite: a separate release authorization. Confirm ownership and availability of `brachio` on crates.io and PyPI, check similar project names, and review native test results and compatibility claims. Follow the [publishing guide](publishing.md) to configure registry credentials and validate the release tag.

The repository supports both registry uploads. No package publication, version tag, or release dispatch was performed during implementation. Packaging checks do not establish native Rex UI acceptance.

## References

- [Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status)
- [Server API](https://www.superlogical.com/rex/docs/automate/server-api)
- [Raw events and Lua events](https://www.superlogical.com/rex/docs/reference/events)
- [Mac actions](https://www.superlogical.com/rex/docs/reference/mac-actions)
