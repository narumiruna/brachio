# Publishing

Brachio uses one version for crates.io and PyPI. `Cargo.toml` is the version source. Maturin reads this version and packages the Rust executable with `bindings = "bin"`. There is no Python CLI implementation or Python library API.

## Change notes

[changenote](https://github.com/narumiruna/changenote) manages pending notes, versions, changelogs, and local annotated tags. It does not publish registry artifacts.

Run the same pinned version used by the PR workflow with uv and Python 3.12 or newer:

```sh
uvx changenote==0.9.2 status
```

No separate tool installation is required. Compatible wheels do not need Rust or Cargo. Use `uvx changenote==0.9.2` in place of `changenote` in the commands below.

Run commands from the repository root. `.changenote/config.json` defines one package, `brachio`, with `Cargo.toml` as its primary version file and `v` as its tag prefix. Do not add the dynamic Python version as an additional version output.

Add or update a note for features, fixes, breaking changes, and installation or other user-facing changes that need a release:

```sh
changenote add fix-navigation --package brachio --bump patch --message "Fix navigation to the selected agent."
changenote status
```

Use `minor` for compatible features, `patch` for compatible fixes, and `major` for breaking changes. Include migration steps for breaking changes. Changenote applies stable SemVer rules even before 1.0.0. Update an existing note for the same change instead of adding a duplicate.

Documentation-only changes, test-only changes, internal refactors, and release-tooling setup need no note unless they affect users or release artifacts. State the reason in the completion report or PR description. Do not create empty notes or invent a bump. Invalid pending notes must be fixed, not deleted or hidden by an exemption.

### PR checks

The `Change notes` workflow runs on pull requests with read-only permissions. It uses `uvx` to run a pinned CLI version from PyPI, not contributor code. Pending releases and PR check results are printed as JSON in the job logs. Keep the CLI and schema pins compatible when upgrading.

The configured check paths are `src/**`, `Cargo.toml`, `Cargo.lock`, `pyproject.toml`, `uv.lock`, and `.github/workflows/publish.yml`. Changes to these paths require a new or updated note for `brachio`. Review release effects outside these paths manually. An unchanged old note does not satisfy a PR check.

For a clean, committed local comparison:

```sh
changenote check --since origin/main --coverage package
```

Fetch the base ref before checking if it is unavailable. The configured default base is `origin/main`; CI uses the actual PR base branch. Uncommitted changes are not checked.

A maintainer can apply the `skip-changenote` label when a change in the checked paths needs no new note. Record the reason in the PR description. This includes internal refactors without release impact and generated release PRs that consume existing notes. Adding or removing the label reruns the check. The exemption removes only the missing-note requirement; configuration, version, and pending-note validation still run.

## Local verification

Install Rust stable, Cargo, uv, and Python 3.12. Run from the repository root:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release
cargo publish --locked --dry-run
uv lock --check
uv build
uvx --isolated --no-index --find-links dist brachio --version
uvx --isolated --no-index --find-links dist brachio --help
```

Use `--allow-dirty` with the Cargo dry run only to check uncommitted local changes. Do not use it for an upload. A dry run does not upload the crate or verify registry credentials.

`uv build` builds a source archive and then builds a wheel from that archive. Wheel installs do not need Rust. Source installs need Rust and Cargo. Python package metadata requires Python 3.10 or newer.

The Checks workflow runs Rust tests and source-to-wheel builds on Linux and macOS. Installed wheel version and help checks do not start the TUI or call Rex.

## Registry setup

A repository maintainer must complete these steps before the first automated release:

1. Confirm ownership and availability of `brachio` on crates.io and PyPI. Check similar names before publication.
2. Create GitHub environments named `crates-io` and `pypi`.
   - Restrict deployments to release tags that start with `v`.
   - Add required reviewers if each registry upload needs approval.
3. Configure [crates.io Trusted Publishing](https://crates.io/docs/trusted-publishing) for `brachio`.
   - The crate must already exist. Its first publication requires a separate, authorized API-token upload by a crate owner. Do not store this token as a GitHub Actions secret.
   - In the crate's Settings → Trusted Publishing, add GitHub repository owner `narumiruna`, repository `brachio`, workflow filename `publish.yml`, and environment `crates-io`.
   - After the initial publication, prepare a new, unused version for the tag-triggered workflow. Do not use the initial published version as the next automated release.
4. Store a PyPI API token as the `PYPI_TOKEN` secret in the `pypi` environment.
   - Before the PyPI project exists, the first upload needs an account-scoped token.
   - After the first upload, replace it with a token scoped to `brachio`.

The crates.io job uses `rust-lang/crates-io-auth-action` to exchange GitHub OIDC identity for a short-lived token. It passes the action's output to Cargo through `CARGO_REGISTRY_TOKEN`; no persistent crates.io secret is required. The action revokes the token when the job ends. Only this job has `id-token: write` permission.

PyPI uses the `PYPI_TOKEN` secret, not Trusted Publishing. Other jobs have only `contents: read` GitHub permission. Do not put token values in source files, issues, or chat.

## Automated publication

The `.github/workflows/publish.yml` workflow starts when a `v*.*.*` tag is pushed. Manual dispatch also requires a version tag; dispatch from a branch fails validation without an upload.

```mermaid
flowchart LR
    Tag[Version tag] --> Validate[Validate version and Rust checks]
    Validate --> Wheels[Build and check all platform wheels]
    Wheels --> Crates[Publish crate to crates.io]
    Wheels --> PyPI[Publish wheels and source archive to PyPI]
```

The release tag must be exactly `v<version>`, where `<version>` is a stable `MAJOR.MINOR.PATCH` version in `Cargo.toml`. The local package version in `Cargo.lock` must match. Prerelease tags are not supported.

The workflow first runs the Rust checks and a crate publication dry run. It then builds and installs each native wheel:

| Platform | Architecture | Runner |
| --- | --- | --- |
| Linux, glibc 2.28 or newer | x86_64 | `ubuntu-latest` |
| Linux, glibc 2.28 or newer | aarch64 | `ubuntu-24.04-arm` |
| macOS | x86_64 | `macos-15-intel` |
| macOS | aarch64 | `macos-14` |

The Linux x86_64 job also builds the source archive. Windows and Alpine/musl wheels are not included. Each installed wheel must report the validated version and accept `--help`. Both upload jobs wait until validation and every wheel build pass. Registry uploads then run independently.

Version changes, local tag creation, tag pushes, GitHub Releases, and registry uploads each need explicit authorization. Adding notes does not authorize these actions. No automatic Release PR or GitHub Release workflow is configured.

To prepare an authorized version change:

1. Start from a clean checkout with committed notes. Run `changenote status` and review the package, bump, summaries, and target version.
2. Populate the package caches with `cargo fetch --locked` and `uv build` before versioning. Changenote refreshes the existing `Cargo.lock` with `cargo update --workspace --offline`.
3. Run `changenote version` only with explicit versioning authorization. It updates `Cargo.toml` and `Cargo.lock`, writes `CHANGELOG.md` and `.changenote/releases.json`, and removes consumed notes. Do not edit the generated release tree by hand.
4. Run `uv lock --offline` separately to refresh Python dependency metadata. The dynamic Python version is not a configured changenote output and need not appear in `uv.lock`. Inspect the diff for unrelated changes.
5. Run `changenote status` again and confirm that the consumed notes are no longer pending. Run the local verification commands and review the Linux and macOS Checks jobs and the [Rex compatibility limits](rex-compatibility.md#external-handoff).
6. Commit only the intended release files, including note deletions, the saved batch, changelog, manifests, and changed lockfiles. If using a release PR, have a maintainer apply `skip-changenote` and explain that existing notes were consumed.

After the release commit is merged and the checkout is clean, create local tags only with separate tag authorization:

```sh
changenote tag
```

This uses the saved release batch to create an annotated `v<version>` tag. Push only the intended tag after separate publication authorization. For version 0.1.0:

```sh
git push origin refs/tags/v0.1.0
```

A normal branch push or pull request does not publish. Do not move a tag after either registry has received its version.

## Partial failures and recovery

The two registry uploads are not a transaction. One can succeed while the other fails.

Use GitHub Actions **Re-run failed jobs** for the same tagged run. This retains successful build artifacts and does not repeat a successful registry job. A manual dispatch on the same tag runs the whole workflow again and can fail if the crate version is already published.

The PyPI upload uses `uv publish --check-url https://pypi.org/simple/`. Matching uploaded files are skipped; files with different contents are rejected. If a rebuilt wheel conflicts, inspect the original artifacts instead of replacing published files.

Crates.io does not permit a second upload of an existing version. If Cargo reports a failure after a possible upload, inspect the registry before retrying. Do not assume that an error means no write occurred.

If local versioning fails, stop and inspect Git state. Changenote attempts to restore its known release files, but recovery is not atomic. Do not discard unrelated changes or retry without checking the result.

Before publication, revert the packaging or workflow change to disable it. After publication, fix a faulty release with a new version. Registry yanks are separate maintainer actions and do not remove installed copies.

## Verification handoff

A maintainer must create and control the `skip-changenote` label, verify the hosted `Change notes` workflow, and select required branch checks. Local tests do not establish GitHub label permissions, branch rules, or hosted Action behavior.

Local verification can establish host-platform Rust behavior, source archive completeness, and installed wheel behavior. It does not verify registry name ownership, the initial crate upload, Trusted Publishing configuration, PyPI tokens, environment approval rules, real uploads, or wheel behavior on other platforms.

The maintainer must verify all four native wheel jobs and both registry uploads during an authorized release. Retain the native Rex UI checks in the compatibility guide; successful packaging does not establish end-to-end Rex support.
