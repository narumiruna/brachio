# Repository instructions

## Documentation

- [UNREVIEWED] Read `docs/publishing.md` before changing release tooling, change notes, or release outputs.

## Boundaries

- [UNREVIEWED] Run `changenote version`, `changenote tag`, tag pushes, GitHub Releases, or registry uploads only when the user explicitly requests that specific release action.

## Git and commits

- [UNREVIEWED] Before committing a release-relevant change, add or update a `brachio` note in `.changenote/` under the policy in `docs/publishing.md#change-notes` and validate it with `changenote status`.
- [UNREVIEWED] For changes that need no release, create no note and state the reason in the completion report.
