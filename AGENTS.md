# Repository instructions

## Documentation

- [UNREVIEWED] Read `docs/publishing.md` before changing release tooling, change notes, or release outputs.

## Rex integration

- Follow the connected Rex server's schema for request arguments, response fields, status meanings, and fallback behavior.
- Before changing Rex integration, inspect the relevant schema with `rex --autostart=false api describe METHOD --json` or `rex --autostart=false block inspect -s SESSION BLOCK --json`.
- For `program_status`:
  - Map unrecognized `state` values to `idle`, as the schema requires; do not add an `Unknown` status.
  - Treat an empty `records` array as no status report, not a reported `unknown` state.
- Keep code, tests, fixtures, and documentation consistent with the verified schema; do not use repository-specific rules to override it.

## Boundaries

- [UNREVIEWED] Run `changenote version`, `changenote tag`, tag pushes, GitHub Releases, or registry uploads only when the user explicitly requests that specific release action.

## Git and commits

- [UNREVIEWED] Before committing a release-relevant change, add or update a `brachio` note in `.changenote/` under the policy in `docs/publishing.md#change-notes` and validate it with `changenote status`.
- [UNREVIEWED] For changes that need no release, create no note and state the reason in the completion report.
