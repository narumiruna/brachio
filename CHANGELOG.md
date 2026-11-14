## 1.2.0

### Minor Changes

- Add independent row sorting with s and group order reversal with Shift+s. Default to discovery order, retain exact selection and folds, and show sorting controls in the TUI.

## 1.1.0

### Minor Changes

- Improve TUI feedback and navigation. Show the latest notice, such as a navigation error, in the status line until the next list input, with `… (w)` when clipped and the full text in Details; keep connection and client problems visible while they persist. Show Connecting before the first snapshot and dim stale rows while disconnected. Move agent counts to the title, followed by colored per-status counts, the grouping mode, and the active filter, and advertise the `a` attention jump in the hints. Select the first match when a filter edit hides the selection. Add status summaries to collapsed or mixed-status group headers, an overflow scrollbar, a CWD heading fallback, and white status labels on the selected row. Add PageUp/PageDown/Home/End list navigation. Escape no longer quits from the list; use q or Ctrl-C. Reorganize Help into sections with word wrapping and Details into labeled sections that start with the selected agent. Preserve all spaces in Details values. Keep group counts and status summaries clear of the scrollbar. Canceling a filter restores the prior selection, including an absent or hidden selection; a removed agent stays unselected. Keep navigation feedback across later refreshes when the selected agent is missing.

## 1.0.0

### Major Changes

- Follow the Rex status schema: use Idle for unrecognized states and foreground-only agents, remove Unknown status, and retain no-report source details.
  
  Breaking change: Remove the public `brachio::model::Status::Unknown` variant.
  
  Migration steps:
  
  - Replace references to `Status::Unknown` with `Status::Idle`.
  - Remove `Status::Unknown` match arms. Handle fallback states in the `Status::Idle` arm.
  - Expect `Status::parse` to return `Status::Idle` for missing or unrecognized states.

## 0.1.0

### Minor Changes

- Add optional Agent, Status, CWD, and Session grouping while keeping None and the original sort as the default. Use g/G to cycle modes and Tab/Shift+Tab to jump between groups. Show foldable headers with filtered counts only for multiple groups; preserve selected IDs through folds and expand without navigation on Enter. Add incremental case-insensitive AND filtering with agent:/cwd:/session:/status: terms, Blocked/Error attention jumps, and scrollable help and diagnostics. Use compact content-measured Status/Agent/CWD/Session/Tab/Pane columns with two-cell gaps and fit-based visibility, display-only HOME abbreviation, status symbols, and a compact bounded footer. Keep selected Idle statuses visible and preserve whitespace-separated pasted filter terms. Keep reported status meanings and exact-ID navigation unchanged.

