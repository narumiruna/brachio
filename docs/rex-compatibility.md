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

The older status schema suggests mapping unknown states to idle. Brachio deliberately maps them to **Unknown**, as required by the plan. Done/Error can survive process exit. A report timestamp is not task duration.

The documented raw terminal-event envelope is:

```json
{"type":"session_event","payload":{"session_id":"session:<uuid>","event":{"type":"block_event","payload":{"block_id":"block:<uuid>","creator_name":"com.superlogical.terminal","name":"program_status_changed","payload":{"id":"","record":{"app":"pi","state":"working"},"reason":"report"}}}}}
```

Only the envelope type is needed: events trigger a new authoritative snapshot, not incremental record edits. CLI client join/leave churn is ignored to prevent snapshot feedback loops. A fixed reconciliation also covers missed events and client changes.

The working-directory column uses `foreground.cwd` only when the foreground process has the same recognized agent identity as the row. It does not use a shell/other program directory for retained agent records. Missing, relative, NUL-containing, or oversized paths remain unknown. Session/tab/pane labels are independent display fields; exact IDs remain navigation data and are available in details.

The server describes session focus as shared session state. A client action can be refused, disconnected, unsupported, or timed out. A timed-out action can still execute later. There is no verified atomic operation that both focuses a block and switches only one app. These limits are documented rather than hidden.

## Automated verification scope

- Model tests: multiple apps, related records, parent inheritance, missing/future states, clear, unsupported apps, safe text, and exact resource IDs. Location labels stay separate from IDs; directory changes cannot change agent identity or navigation targets.
- Mock CLI tests: multiple sessions/tabs/panes, detached and nonterminal blocks, empty status, retained completed reports, malformed JSON, unsupported status, ambiguous locations, pinned app choice, transient startup discovery recovery, refusal to replace a pin, precise ordered arguments, stale/moved panes, focus failure, permission refusal, event refresh, reconnect, navigation during a slow scan, and worker shutdown.
- TUI tests: empty and long lists, status groups, readable location columns, working-directory path clipping, Unicode clipping, tiny viewports, scroll, resize, stable selection, removed targets, stale click maps, keyboard/mouse target equivalence, disabled navigation, and complete scrollable message/warning/selected-agent details.
- Subprocess tests: timeout, cancellation, argument boundaries, invalid encoding, output limits, stderr failures, stream framing, EOF, and owned descendant cleanup.
- PTY smoke tests: actual binary startup and keyboard/signal exits with terminal-mode and escape cleanup, including SIGTERM during sustained input. Nonterminal and argument errors exit without starting the TUI.

### Local results

All four standard checks passed on macOS with the measured Rust toolchain. The suite has 101 tests: 71 library tests, one redraw/input routing test, 19 mock Rex integration tests, and 10 PTY/CLI smoke tests. Three additional complete test runs passed before the location/working-directory update. Release binary help/version checks also passed without terminal setup or Rex navigation.

Repeated verification exposed and corrected a Darwin process-exit transition where group signaling can report EPERM before waitid exposes exit. The runner retains child identity during a bounded retry; a 100-short-stream regression checks this path. Process and mock CLI fixtures use serialized execution to avoid cold-executable launch saturation. Worker concurrency remains active within each fixture, including the slow-scan navigation test. Process fixtures also use startup-specific test deadlines; production request deadlines remain unchanged.

Source review also corrected queued-click retargeting, duplicate-parent record inheritance, input-flood shutdown, and final terminal cleanup after a worker panic. Automated PR review identified non-mutating mouse events that consumed click targets, hidden warning details, startup discovery that did not recover, and navigation delayed by snapshot scans. The fixes preserve motion/release hit targets, expose scrollable details, retry only transient pre-pin discovery, and separate serialized navigation from read-only refresh work. No live app switch or agent input was sent during verification. The CI workflow repeats the standard checks on Linux and macOS; native UI acceptance remains a separate handoff.

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
4. Close the selected pane. Confirm that the entry disappears and Enter cannot silently select another row.
5. Disconnect/reconnect Rex. Confirm stale indication, navigation blocking, and eventual resynchronization.
6. Test with Remote Control disabled and with multiple app clients. Confirm refusal or explicit client selection, without fallback.
7. Observe another client attached to the same test session and document shared focus effects.
8. Exit Brachio. Confirm that normal terminal input, mouse behavior, screen, and cursor return.

Expected result: only the requested navigation occurs; no prompt, key, agent command, pane closure, or permission change is sent.

Unverified: physical mouse delivery, visible app switching, return-to-TUI behavior, native resize, and shared-client UI effects. **Do not claim native end-to-end platform acceptance before these checks pass.**

### Package maintainer: release

Prerequisite: a separate release authorization. Recheck `brachio` on crates.io and similar project names, review native test results and compatibility claims, then decide whether to release.

No package publication, registry credential request, version tag, or release dispatch is authorized by this work. The manifest's `publish = false` is intentional.

## References

- [Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status)
- [Server API](https://www.superlogical.com/rex/docs/automate/server-api)
- [Raw events and Lua events](https://www.superlogical.com/rex/docs/reference/events)
- [Mac actions](https://www.superlogical.com/rex/docs/reference/mac-actions)
