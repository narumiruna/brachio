"""Check release routing and data flow without publication or network writes."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
import yaml

ROOT = Path(__file__).resolve().parents[2]


class ReleaseWorkflow(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = yaml.load(
            (ROOT / ".github/workflows/release.yml").read_text(),
            Loader=yaml.BaseLoader,
        )
        cls.jobs = cls.workflow["jobs"]

    def step(self, job, name):
        return next(step for step in self.jobs[job]["steps"] if step.get("name") == name)

    def test_pr_note_script_is_wired_and_valid_bash(self):
        step = self.step("notes", "Check PR notes")
        self.assertEqual(step["shell"], "bash")
        self.assertEqual(step["run"], "bash .github/scripts/check-pr-notes.sh")
        script = ROOT / ".github/scripts/check-pr-notes.sh"
        self.assertNotIn("${{", script.read_text())
        result = subprocess.run(["bash", "-n", str(script)],
                                text=True, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.step("notes", "Check PR notes")["env"], {
            "BASE_BRANCH": "${{ github.base_ref }}",
        })
        self.assertNotIn("labels", json.dumps(self.jobs["notes"]))

    def test_event_routing_and_manual_default(self):
        events = self.workflow["on"]
        self.assertEqual(set(events), {"pull_request", "push", "workflow_dispatch"})
        self.assertEqual(events["push"], {"branches": ["main"]})
        self.assertEqual(events["pull_request"]["types"],
                         ["opened", "synchronize", "reopened"])
        operation = events["workflow_dispatch"]["inputs"]["operation"]
        self.assertEqual(operation["default"], "manage-pr")
        self.assertEqual(operation["options"], ["manage-pr", "retry-publication"])
        self.assertEqual(self.jobs["notes"]["if"], "github.event_name == 'pull_request'")
        self.assertEqual(self.jobs["release"]["if"],
                         "github.ref == 'refs/heads/main' && github.event_name != 'pull_request'")
        self.assertEqual(self.workflow["concurrency"],
                         {"group": "release-${{ github.ref }}", "cancel-in-progress": "false"})

    def test_secret_and_permission_boundaries(self):
        self.assertEqual(self.workflow["permissions"], {"contents": "read"})
        for name, job in self.jobs.items():
            with self.subTest(job=name):
                permissions = job.get("permissions", self.workflow["permissions"])
                self.assertEqual(permissions.get("id-token", "none"),
                                 "write" if name == "crates-io" else "none")
                if name != "release":
                    self.assertNotIn("PAT_TOKEN", json.dumps(job))
                for step in job["steps"]:
                    if step.get("uses", "").startswith("actions/checkout@"):
                        self.assertEqual(step["with"]["persist-credentials"], "false")
        action = self.jobs["release"]["steps"][-1]
        self.assertEqual(action["uses"],
                         "narumiruna/changenote/release@f4423760122463bf03a8d161c6f0bc4173afb6e0")
        self.assertEqual(action["with"], {
            "github-token": "${{ secrets.PAT_TOKEN }}", "base-branch": "main",
            "operation": "${{ inputs.operation || 'auto' }}",
        })
        self.assertEqual(self.jobs["release"]["outputs"], {
            "release-commit": "${{ steps.release.outputs.release-commit }}",
            "version": "${{ steps.release.outputs.release-commit != '' && fromJSON(steps.release.outputs.releases)[0].version || '' }}",
        })
        self.assertEqual(action["id"], "release")

    def test_registry_jobs_use_verified_commit_and_build_gates(self):
        self.assertEqual(self.jobs["validate"]["needs"], "release")
        self.assertEqual(self.jobs["validate"]["if"], "needs.release.outputs.release-commit != ''")
        self.assertEqual(self.jobs["wheels"]["needs"], ["release", "validate"])
        self.assertEqual(self.jobs["crates-io"]["needs"], ["release", "validate", "wheels"])
        self.assertEqual(self.jobs["pypi"]["needs"], ["validate", "wheels"])
        self.assertEqual(self.step("wheels", "Check installed wheel")["env"],
                         {"EXPECTED_VERSION": "${{ needs.release.outputs.version }}"})
        self.assertNotIn("outputs", self.jobs["validate"])
        for name in ("validate", "wheels", "crates-io"):
            checkout = self.jobs[name]["steps"][0]
            self.assertEqual(checkout["with"]["ref"], "${{ needs.release.outputs.release-commit }}")
        self.assertEqual(self.step("wheels", "Build source distribution")["if"],
                         "matrix.target == 'x86_64-unknown-linux-gnu'")
        for name in ("crates-io", "pypi"):
            self.assertEqual(self.jobs[name]["environment"]["name"], name)

    def test_wheel_matrix_uses_supported_native_runners(self):
        self.assertEqual(self.jobs["wheels"]["runs-on"], "${{ matrix.runner }}")
        matrix = self.jobs["wheels"]["strategy"]["matrix"]["include"]
        self.assertEqual(matrix, [
            {"runner": "ubuntu-latest", "target": "x86_64-unknown-linux-gnu"},
            {"runner": "ubuntu-24.04-arm", "target": "aarch64-unknown-linux-gnu"},
            {"runner": "macos-15-intel", "target": "x86_64-apple-darwin"},
            {"runner": "macos-15", "target": "aarch64-apple-darwin"},
        ])

    def test_obsolete_workflows_and_note_pattern(self):
        for name in ("changenote.yml", "publish.yml"):
            self.assertFalse((ROOT / ".github/workflows" / name).exists())
        patterns = json.loads((ROOT / ".changenote/config.json").read_text())["changed_file_patterns"]
        self.assertIn(".github/workflows/release.yml", patterns)
        self.assertNotIn(".github/workflows/publish.yml", patterns)

    def test_pat_guard_fails_without_disclosing_token(self):
        steps = self.jobs["release"]["steps"]
        self.assertEqual(steps[0]["name"], "Check release token")
        self.assertTrue(steps[1]["uses"].startswith("actions/checkout@"))
        script = self.step("release", "Check release token")["run"]
        for token, expected in ((None, 1), ("", 1), ("fixture-token-do-not-log", 0)):
            with self.subTest(token_present=bool(token)):
                env = dict(os.environ)
                env.pop("PAT_TOKEN", None)
                if token is not None:
                    env["PAT_TOKEN"] = token
                result = subprocess.run(["bash", "-e", "-o", "pipefail", "-c", script],
                                        env=env, text=True, capture_output=True, timeout=10)
                self.assertEqual(result.returncode, expected, result.stderr)
                if token:
                    self.assertNotIn(token, result.stdout + result.stderr)
                else:
                    self.assertIn("Set the PAT_TOKEN repository secret", result.stdout)

    def run_note_check(self, base="main", changes=None, event=None, ref="main",
                       cli_exit=0, real_cli=False):
        step = self.step("notes", "Check PR notes")
        self.assertNotIn("${{", step["run"])
        with tempfile.TemporaryDirectory() as temporary:
            top = Path(temporary)
            repo = top / "repo"
            repo.mkdir()
            env = dict(os.environ, GIT_AUTHOR_NAME="Workflow fixture",
                       GIT_AUTHOR_EMAIL="fixture@example.invalid",
                       GIT_COMMITTER_NAME="Workflow fixture",
                       GIT_COMMITTER_EMAIL="fixture@example.invalid",
                       GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")

            def git(*args):
                subprocess.run(["git", *args], cwd=repo, env=env, check=True,
                               text=True, capture_output=True, timeout=10)

            files = {
                "src/main.rs": "fn main() {}\n",
                "Cargo.toml": '[package]\nname = "brachio"\nversion = "0.1.0"\n',
                "Cargo.lock": 'version = 4\n[[package]]\nname = "brachio"\nversion = "0.1.0"\n',
                "CHANGELOG.md": "# Changelog\n",
                ".changenote/config.json": (ROOT / ".changenote/config.json").read_text(),
                ".changenote/entry.md": "---\nbrachio: patch\n---\n\nFix an issue.\n",
                ".changenote/releases.json": "[]\n",
            }
            for path, content in files.items():
                target = repo / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text(content)
            git("init", "-q", "-b", "main")
            git("add", "--", *files)
            git("commit", "-q", "-m", "fixture: base")
            git("update-ref", "refs/remotes/origin/" + ref, "HEAD")
            if changes is None:
                changes = {"src/main.rs": "fn main() { println!(\"changed\"); }\n"}
            for path, content in changes.items():
                target = repo / path
                if content is None:
                    target.unlink()
                else:
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_text(content)
            if changes:
                git("add", "--", *changes)
                git("commit", "-q", "-m", "fixture: change")
            calls = top / "calls"
            uvx = top / "uvx"
            uvx.write_text('#!/usr/bin/env bash\nprintf "%s\\0" "$@" > "$CALLS"\nexit "$CLI_EXIT"\n')
            uvx.chmod(0o755)
            if not real_cli:
                env["PATH"] = str(top) + os.pathsep + os.environ["PATH"]
            # The legacy variable must not control automatic classification.
            env.update(CALLS=str(calls), BASE_BRANCH=base, CLI_EXIT=str(cli_exit),
                       SKIP_RELEASE="true")
            env.pop("GITHUB_EVENT_PATH", None)
            env.pop("GITHUB_REPOSITORY", None)
            if event is not None:
                event_path = top / "event.json"
                event_path.write_text(json.dumps(event))
                env.update(GITHUB_EVENT_PATH=str(event_path), GITHUB_REPOSITORY="narumiruna/brachio")
            script = ROOT / ".github/scripts/check-pr-notes.sh"
            result = subprocess.run(["bash", str(script)], cwd=repo, env=env,
                                    text=True, capture_output=True, timeout=30)
            args = calls.read_bytes().decode().split("\0")[:-1] if calls.exists() else []
            self.assertFalse((repo / "injected").exists())
            return result, args

    def test_pr_arguments_keep_package_coverage(self):
        result, args = self.run_note_check("main")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(args, ["changenote==0.9.2", "check", "--since=refs/remotes/origin/main", "--coverage=package"])

    def test_non_release_paths_automatically_exempt_missing_entries(self):
        for path in (".github/workflows/release.yml", ".github/scripts/helper.sh",
                     "docs/guide.md", "tests/test.py", "README.md", "AGENTS.md",
                     ".changenote/README.md", "docs/line\nname with spaces.md"):
            with self.subTest(path=path):
                result, args = self.run_note_check(changes={path: "updated\n"})
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("--coverage=package", args)
                self.assertEqual(args[-3:-1], ["--skip-release", "--reason"])
                self.assertTrue(args[-1].startswith("Only documentation"))

    def test_source_manifests_and_unknown_paths_keep_normal_coverage(self):
        for path in ("src/main.rs", "Cargo.toml", "Cargo.lock", "pyproject.toml",
                     "uv.lock", "unknown/path", "runtime\nfile.rs"):
            with self.subTest(path=path):
                result, args = self.run_note_check(changes={path: "updated\n"})
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertNotIn("--skip-release", args)

    def test_mixed_source_and_ci_changes_keep_normal_coverage(self):
        result, args = self.run_note_check(changes={"src/main.rs": "updated\n",
                                                    ".github/workflows/release.yml": "updated\n"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--skip-release", args)

    def test_source_renamed_into_docs_keeps_normal_coverage(self):
        result, args = self.run_note_check(changes={"src/main.rs": None,
                                                    "docs/main.rs": "fn main() {}\n"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--skip-release", args)

    def test_empty_diff_uses_normal_coverage(self):
        result, args = self.run_note_check(changes={})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--skip-release", args)

    def test_cli_failure_is_not_hidden_by_automatic_exemption(self):
        result, args = self.run_note_check(changes={"docs/guide.md": "updated\n"}, cli_exit=42)
        self.assertEqual(result.returncode, 42, result.stderr)
        self.assertIn("--skip-release", args)

    def test_unknown_base_fails_before_cli(self):
        result, args = self.run_note_check(base="missing")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(args, [])

    def test_pr_branch_is_data_not_shell_syntax(self):
        base = "topic/odd;$(touch${IFS}injected)"
        result, args = self.run_note_check(base=base, ref=base)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--since=refs/remotes/origin/" + base, args)

    def test_empty_pr_base_fails_before_cli(self):
        result, args = self.run_note_check("")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("PR base branch is required", result.stderr)
        self.assertEqual(args, [])

    def release_changes(self):
        return {
            ".changenote/releases.json": json.dumps([{
                "package": "brachio", "old_version": "0.1.0", "version": "0.1.1",
                "bump": "patch", "tag": "v0.1.1",
            }]),
            ".changenote/entry.md": None,
            "Cargo.toml": '[package]\nname = "brachio"\nversion = "0.1.1"\n',
            "Cargo.lock": 'version = 4\n[[package]]\nname = "brachio"\nversion = "0.1.1"\n',
            "CHANGELOG.md": "# Changelog\n\n## 0.1.1\n\nFix an issue.\n",
        }

    def managed_release_event(self):
        return {"pull_request": {
            "base": {"ref": "main", "repo": {"full_name": "narumiruna/brachio"}},
            "head": {"ref": "narumi/chore/changenote-release-main",
                     "repo": {"full_name": "narumiruna/brachio"}},
            "body": "<!-- changenote-release-pr -->\n\nRelease details.",
        }}

    def test_managed_release_outputs_automatically_exempt_missing_entries(self):
        result, args = self.run_note_check(changes=self.release_changes(),
                                          event=self.managed_release_event())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--skip-release", args)
        self.assertTrue(args[-1].startswith("Managed Release PR"))

    def test_release_outputs_without_managed_event_keep_normal_coverage(self):
        result, args = self.run_note_check(changes=self.release_changes())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--skip-release", args)

    def test_unrecognized_release_prs_keep_normal_coverage(self):
        for field in ("fork", "branch", "body", "base", "base_repo"):
            with self.subTest(field=field):
                event = self.managed_release_event()
                pr = event["pull_request"]
                if field == "fork":
                    pr["head"]["repo"]["full_name"] = "other/brachio"
                elif field == "branch":
                    pr["head"]["ref"] = "feature"
                elif field == "body":
                    pr["body"] = "Unmarked release"
                elif field == "base":
                    pr["base"]["ref"] = "other"
                else:
                    pr["base"]["repo"]["full_name"] = "other/brachio"
                result, args = self.run_note_check(changes=self.release_changes(), event=event)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertNotIn("--skip-release", args)

    def test_managed_release_with_source_changes_keeps_normal_coverage(self):
        changes = self.release_changes()
        changes["src/main.rs"] = "updated\n"
        result, args = self.run_note_check(changes=changes, event=self.managed_release_event())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--skip-release", args)

    def test_managed_release_without_updated_batch_keeps_normal_coverage(self):
        changes = self.release_changes()
        del changes[".changenote/releases.json"]
        result, args = self.run_note_check(changes=changes, event=self.managed_release_event())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--skip-release", args)

    def test_managed_release_with_deleted_batch_keeps_normal_coverage(self):
        changes = self.release_changes()
        changes[".changenote/releases.json"] = None
        result, args = self.run_note_check(changes=changes, event=self.managed_release_event())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--skip-release", args)

    def run_wheel_check(self, expected="0.1.0", actual="brachio 0.1.0",
                        version_exit=0, help_exit=0):
        with tempfile.TemporaryDirectory() as temporary:
            top = Path(temporary)
            calls = top / "calls"
            uvx = top / "uvx"
            uvx.write_text(
                '#!/usr/bin/env bash\n'
                'printf "%s\\0" "--call--" "$@" >> "$CALLS"\n'
                'case "${!#}" in\n'
                '  --version) printf "%s\\n" "$VERSION_OUTPUT"; exit "$VERSION_EXIT" ;;\n'
                '  --help) exit "$HELP_EXIT" ;;\n'
                '  *) exit 99 ;;\n'
                'esac\n'
            )
            uvx.chmod(0o755)
            env = dict(os.environ, PATH=str(top) + os.pathsep + os.environ["PATH"],
                       CALLS=str(calls), EXPECTED_VERSION=expected, VERSION_OUTPUT=actual,
                       VERSION_EXIT=str(version_exit), HELP_EXIT=str(help_exit))
            script = self.step("wheels", "Check installed wheel")["run"]
            self.assertNotIn(".github/scripts/", script)
            result = subprocess.run(["bash", "-e", "-o", "pipefail", "-c", script],
                                    cwd=top, env=env, text=True, capture_output=True, timeout=10)
            args = [call.decode().split("\0")[:-1]
                    for call in calls.read_bytes().split(b"--call--\0")[1:]] if calls.exists() else []
            return result, args

    def test_wheel_check_uses_local_artifacts_without_repository_scripts(self):
        result, calls = self.run_wheel_check()
        self.assertEqual(result.returncode, 0, result.stderr)
        prefix = ["--isolated", "--no-index", "--find-links", "dist", "brachio"]
        self.assertEqual(calls, [prefix + ["--version"], prefix + ["--help"]])

    def test_wheel_version_mismatch_stops_before_help(self):
        result, calls = self.run_wheel_check(actual="brachio 9.9.9")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][-1], "--version")

    def test_wheel_version_command_failure_stops_before_help(self):
        result, calls = self.run_wheel_check(version_exit=42)
        self.assertEqual(result.returncode, 42, result.stderr)
        self.assertEqual(len(calls), 1)

    def test_wheel_help_command_failure_is_preserved(self):
        result, calls = self.run_wheel_check(help_exit=43)
        self.assertEqual(result.returncode, 43, result.stderr)
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[-1][-1], "--help")


if __name__ == "__main__":
    unittest.main()
