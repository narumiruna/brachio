"""Check release routing and data flow without publication or network writes."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
import urllib.request

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
        cls.build_workflow = yaml.load(
            (ROOT / ".github/workflows/build-release.yml").read_text(),
            Loader=yaml.BaseLoader,
        )
        cls.build_jobs = cls.build_workflow["jobs"]

    def step(self, job, name):
        jobs = self.build_jobs if job in self.build_jobs else self.jobs
        return next(step for step in jobs[job]["steps"] if step.get("name") == name)

    def test_pr_note_policy_and_check_action_are_wired(self):
        policy = self.step("notes", "Select note policy")
        self.assertEqual(policy["id"], "note-policy")
        self.assertEqual(policy["shell"], "bash")
        self.assertEqual(policy["run"], "bash .github/scripts/pr-note-policy.sh")
        self.assertEqual(policy["env"], {"BASE_BRANCH": "${{ github.base_ref }}"})
        script = ROOT / ".github/scripts/pr-note-policy.sh"
        self.assertNotIn("${{", script.read_text())
        self.assertNotIn("uvx", script.read_text())
        result = subprocess.run(["bash", "-n", str(script)],
                                text=True, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        action = self.step("notes", "Check PR notes")
        self.assertEqual(action["uses"],
                         "changenote/action/check@b5a808ce21642ccc01587dc66fd34ce522b74da9")
        self.assertEqual(action["with"], {
            "since": "refs/remotes/origin/${{ github.base_ref }}",
            "coverage": "package",
            "skip-release": "${{ steps.note-policy.outputs.skip-release }}",
            "reason": "${{ steps.note-policy.outputs.reason }}",
        })
        self.assertNotIn("if", action)
        self.assertNotIn("continue-on-error", action)
        self.assertEqual(len(self.jobs["notes"]["steps"]), 3)
        self.assertNotIn("labels", json.dumps(self.jobs["notes"]))

    def test_event_routing_and_manual_pr_management(self):
        events = self.workflow["on"]
        self.assertEqual(set(events), {"pull_request", "push", "workflow_dispatch"})
        self.assertEqual(events["push"], {"branches": ["main"]})
        self.assertEqual(events["pull_request"]["types"],
                         ["opened", "synchronize", "reopened"])
        self.assertEqual(events["workflow_dispatch"], "")
        self.assertEqual(self.jobs["notes"]["if"], "github.event_name == 'pull_request'")
        self.assertEqual(self.jobs["release"]["if"],
                         "github.ref == 'refs/heads/main' && github.event_name != 'pull_request'")
        self.assertEqual(self.workflow["concurrency"],
                         {"group": "release-${{ github.ref }}", "cancel-in-progress": "false"})

    def test_secret_and_permission_boundaries(self):
        self.assertEqual(self.workflow["permissions"], {"contents": "read"})
        self.assertEqual(self.build_workflow["permissions"], {"contents": "read"})
        for workflow in (self.workflow, self.build_workflow):
            for name, job in workflow["jobs"].items():
                with self.subTest(job=name):
                    permissions = job.get("permissions", workflow["permissions"])
                    self.assertEqual(permissions.get("id-token", "none"),
                                     "write" if name == "crates-io" else "none")
                    if name != "release":
                        self.assertNotIn("PAT_TOKEN", json.dumps(job))
                    for step in job.get("steps", []):
                        if step.get("uses", "").startswith("actions/checkout@"):
                            self.assertEqual(step["with"]["persist-credentials"], "false")
        build = json.dumps(self.build_workflow)
        for secret in ("PAT_TOKEN", "PYPI_TOKEN", "CARGO_REGISTRY_TOKEN"):
            self.assertNotIn(secret, build)
        self.assertNotIn("secrets", self.jobs["build"])
        for job in self.build_jobs.values():
            self.assertNotIn("environment", job)
            self.assertNotIn("concurrency", job)

    def test_release_uses_one_root_action(self):
        steps = self.jobs["release"]["steps"]
        self.assertEqual(steps[1]["with"]["ref"], "${{ github.sha }}")
        self.assertEqual(steps[1]["with"]["fetch-depth"], "0")
        self.assertEqual(len(steps), 3)
        action = self.step("release", "Manage release")
        self.assertEqual(action["uses"],
                         "changenote/action@b5a808ce21642ccc01587dc66fd34ce522b74da9")
        self.assertEqual(action["id"], "release")
        self.assertEqual(action["with"], {
            "github-token": "${{ secrets.PAT_TOKEN }}",
            "base-branch": "main",
            "push-git-tags": "true",
            "create-github-releases": "true",
        })
        self.assertNotIn("if", action)
        self.assertNotIn("continue-on-error", action)

    def test_release_action_failure_stops_downstream_work(self):
        for job in self.jobs.values():
            self.assertNotIn("continue-on-error", job)
            self.assertNotIn("always()", job.get("if", ""))
            for step in job.get("steps", []):
                self.assertNotIn("continue-on-error", step)
        self.assertNotIn("Check release result", json.dumps(self.workflow))
        self.assertEqual(self.jobs["build"]["needs"], "release")
        self.assertEqual(self.jobs["crates-io"]["needs"], ["release", "build"])
        self.assertEqual(self.jobs["pypi"]["needs"], "build")

    def test_actions_own_cli_setup_and_reports(self):
        for job, name in (("notes", "Check PR notes"), ("release", "Manage release")):
            action = self.step(job, name)
            self.assertNotIn("version", action["with"])
            self.assertNotIn("cli-path", action["with"])
        self.assertNotIn("uvx", json.dumps(self.jobs["notes"]))

    def test_root_outputs_preserve_publication_state(self):
        self.assertEqual(self.jobs["release"]["outputs"], {
            "publication-needed": "${{ steps.release.outputs.publication-needed }}",
            "release-commit": "${{ steps.release.outputs.release-commit }}",
            "version": "${{ steps.release.outputs.release-commit != '' && fromJSON(steps.release.outputs.releases)[0].version || '' }}",
        })

    def test_registry_jobs_use_verified_commit_and_build_gates(self):
        build = self.jobs["build"]
        self.assertEqual(build["needs"], "release")
        self.assertEqual(build["if"],
                         "needs.release.outputs.publication-needed == 'true' && needs.release.outputs.release-commit != ''")
        self.assertEqual(build["uses"], "./.github/workflows/build-release.yml")
        self.assertEqual(build["with"], {
            "release-commit": "${{ needs.release.outputs.release-commit }}",
            "version": "${{ needs.release.outputs.version }}",
        })
        self.assertEqual(set(self.build_workflow["on"]), {"workflow_call"})
        inputs = self.build_workflow["on"]["workflow_call"]["inputs"]
        self.assertEqual(set(inputs), {"release-commit", "version"})
        for value in inputs.values():
            self.assertEqual(value["type"], "string")
            self.assertEqual(value["required"], "true")
        self.assertEqual(set(self.build_jobs), {"validate", "wheels"})
        self.assertNotIn("needs", self.build_jobs["validate"])
        self.assertEqual(self.build_jobs["wheels"]["needs"], "validate")
        self.assertEqual(self.build_jobs["wheels"]["strategy"]["fail-fast"], "false")
        self.assertEqual(self.jobs["crates-io"]["needs"], ["release", "build"])
        self.assertEqual(self.jobs["pypi"]["needs"], "build")
        self.assertEqual(self.step("wheels", "Check installed wheel")["env"],
                         {"EXPECTED_VERSION": "${{ inputs.version }}"})
        for job in self.build_jobs.values():
            self.assertNotIn("outputs", job)
            self.assertEqual(job["steps"][0]["with"]["ref"], "${{ inputs.release-commit }}")
        checkout = self.jobs["crates-io"]["steps"][0]
        self.assertEqual(checkout["with"]["ref"], "${{ needs.release.outputs.release-commit }}")
        self.assertEqual(self.step("wheels", "Build source distribution")["if"],
                         "matrix.target == 'x86_64-unknown-linux-gnu'")
        for name in ("crates-io", "pypi"):
            self.assertEqual(self.jobs[name]["environment"]["name"], name)

    def test_artifacts_stay_in_the_current_workflow_run(self):
        upload = self.build_jobs["wheels"]["steps"][-1]
        self.assertEqual(upload["with"], {
            "name": "python-${{ matrix.target }}",
            "path": "dist/*",
            "if-no-files-found": "error",
        })
        download = self.jobs["pypi"]["steps"][0]
        self.assertTrue(download["uses"].startswith("actions/download-artifact@"))
        self.assertEqual(download["with"], {
            "pattern": "python-*",
            "merge-multiple": "true",
            "path": "dist",
        })

    def test_wheel_matrix_uses_supported_native_runners(self):
        self.assertEqual(self.build_jobs["wheels"]["runs-on"], "${{ matrix.runner }}")
        matrix = self.build_jobs["wheels"]["strategy"]["matrix"]["include"]
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
                       cli_exit=0, real_cli=False, legacy_batch=True, check_script=None):
        step = self.step("notes", "Select note policy")
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
            if not legacy_batch:
                del files[".changenote/releases.json"]
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
            outputs = top / "outputs"
            env.update(CALLS=str(calls), BASE_BRANCH=base, CLI_EXIT=str(cli_exit),
                       GITHUB_OUTPUT=str(outputs), SKIP_RELEASE="true")
            env.pop("GITHUB_EVENT_PATH", None)
            env.pop("GITHUB_REPOSITORY", None)
            if event is not None:
                event_path = top / "event.json"
                event_path.write_text(json.dumps(event))
                env.update(GITHUB_EVENT_PATH=str(event_path), GITHUB_REPOSITORY="narumiruna/brachio")
            script = ROOT / ".github/scripts/pr-note-policy.sh"
            result = subprocess.run(["bash", str(script)], cwd=repo, env=env,
                                    text=True, capture_output=True, timeout=30)
            if result.returncode == 0:
                policy = dict(line.split("=", 1) for line in outputs.read_text().splitlines())
                self.assertEqual(set(policy), {"skip-release", "reason"})
                self.assertIn(policy["skip-release"], ("true", "false"))
                self.assertEqual(bool(policy["reason"]), policy["skip-release"] == "true")
                # Exercise the check Action's CLI contract with the policy outputs.
                args = ["changenote", "check", "--since=refs/remotes/origin/" + base,
                        "--coverage=package"]
                if policy["skip-release"] == "true":
                    args += ["--skip-release", "--reason", policy["reason"]]
                if check_script is not None:
                    self.assertTrue(real_cli)
                    tools = top / "action-tools"
                    tools.mkdir()
                    cli = tools / "changenote"
                    cli.write_text('#!/usr/bin/env bash\nexec uvx changenote "$@"\n')
                    cli.chmod(0o755)
                    if os.uname().sysname != "Linux":
                        # Only the platform precondition is mocked on macOS.
                        # The pinned check script and CLI run without changes.
                        uname = tools / "uname"
                        uname.write_text('#!/bin/sh\nprintf "Linux\\n"\n')
                        uname.chmod(0o755)
                    action_outputs = top / "action-outputs"
                    env.update(PATH=str(tools) + os.pathsep + env["PATH"],
                               GITHUB_OUTPUT=str(action_outputs),
                               GITHUB_STEP_SUMMARY=str(top / "summary"),
                               INPUT_SINCE="refs/remotes/origin/" + base,
                               INPUT_COVERAGE="package",
                               INPUT_SKIP_RELEASE=policy["skip-release"],
                               INPUT_REASON=policy["reason"])
                    for key in ("GH_TOKEN", "GITHUB_TOKEN", "PAT_TOKEN"):
                        env.pop(key, None)
                    result = subprocess.run(["bash", str(check_script)], cwd=repo, env=env,
                                            text=True, capture_output=True, timeout=60)
                    report = dict(line.split("=", 1) for line in
                                  action_outputs.read_text().splitlines()) if action_outputs.exists() else {}
                    return result, report
                result = subprocess.run(["uvx", *args], cwd=repo, env=env,
                                        text=True, capture_output=True, timeout=30)
            args = calls.read_bytes().decode().split("\0")[:-1] if calls.exists() else []
            self.assertFalse((repo / "injected").exists())
            return result, args

    def test_pr_arguments_keep_package_coverage(self):
        result, args = self.run_note_check("main")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(args, ["changenote", "check", "--since=refs/remotes/origin/main", "--coverage=package"])

    def test_non_release_paths_automatically_exempt_missing_entries(self):
        for path in (".github/workflows/release.yml", ".github/scripts/helper.sh",
                     "docs/guide.md", "tests/test.py", "README.md", "AGENTS.md", "prek.toml",
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

    def test_mixed_source_and_prek_changes_keep_normal_coverage(self):
        result, args = self.run_note_check(changes={"src/main.rs": "updated\n",
                                                    "prek.toml": "updated\n"})
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

    def test_pinned_check_action_contract(self):
        action = self.step("notes", "Check PR notes")["uses"]
        revision = action.split("@", 1)[1]
        url = ("https://raw.githubusercontent.com/changenote/action/"
               + revision + "/scripts/action/check.sh")
        with tempfile.TemporaryDirectory() as temporary:
            script = Path(temporary) / "check.sh"
            with urllib.request.urlopen(url, timeout=30) as response:
                script.write_bytes(response.read())
            cases = [
                ("normal", {"changes": {
                    "src/main.rs": "fn main() { println!(\"changed\"); }\n",
                    ".changenote/entry.md": "---\nbrachio: patch\n---\n\nUpdated fix.\n",
                }}, True),
                ("exempt", {"changes": {"docs/guide.md": "Updated.\n"}}, True),
                ("missing note", {}, False),
                ("invalid note", {"changes": {
                    ".changenote/entry.md": "---\nbrachio: invalid\n---\n\nInvalid.\n",
                }}, None),
            ]
            for name, kwargs, passed in cases:
                with self.subTest(case=name):
                    result, outputs = self.run_note_check(
                        real_cli=True, check_script=script, **kwargs
                    )
                    self.assertEqual(result.returncode == 0, passed is True,
                                     result.stdout + result.stderr)
                    if passed is None:
                        self.assertEqual(outputs, {})
                        self.assertIn("No report outputs are available", result.stdout)
                    else:
                        self.assertEqual(set(outputs), {"check-report", "release-plan", "passed"})
                        self.assertEqual(outputs["passed"], str(passed).lower())
                        self.assertEqual(json.loads(outputs["check-report"])["passed"], passed)
                        plan = json.loads(outputs["release-plan"])
                        self.assertEqual(plan[0]["package"], "brachio")
                        self.assertEqual(plan[0]["note_paths"], [".changenote/entry.md"])

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

    def managed_release_event(self, base="main"):
        return {"pull_request": {
            "base": {"ref": base, "repo": {"full_name": "narumiruna/brachio"}},
            "head": {"ref": "changenote-release/" + base,
                     "repo": {"full_name": "narumiruna/brachio"}},
            "body": "<!-- changenote-release-pr -->\n\nRelease details.",
        }}

    def test_managed_release_outputs_automatically_exempt_missing_entries(self):
        result, args = self.run_note_check(changes=self.release_changes(),
                                          event=self.managed_release_event())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--skip-release", args)
        self.assertTrue(args[-1].startswith("Managed Release PR"))

    def test_legacy_release_branch_exempts_missing_entries(self):
        event = self.managed_release_event()
        event["pull_request"]["head"]["ref"] = (
            "narumi/chore/changenote-release-main"
        )
        result, args = self.run_note_check(
            changes=self.release_changes(), event=event
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--skip-release", args)

    def test_managed_release_uses_selected_base(self):
        base = "stable/1.x"
        result, args = self.run_note_check(
            base=base, ref=base, changes=self.release_changes(),
            event=self.managed_release_event(base)
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--since=refs/remotes/origin/" + base, args)
        self.assertIn("--skip-release", args)

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

    def test_managed_release_with_other_paths_keeps_normal_coverage(self):
        for path in ("pyproject.toml", "uv.lock", ".github/workflows/release.yml",
                     ".changenote/config.json", "unknown/path"):
            with self.subTest(path=path):
                changes = self.release_changes()
                changes[path] = "updated\n"
                result, args = self.run_note_check(
                    changes=changes, event=self.managed_release_event()
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertNotIn("--skip-release", args)

    def test_managed_release_with_unchanged_batch_exempts_missing_entries(self):
        changes = self.release_changes()
        del changes[".changenote/releases.json"]
        result, args = self.run_note_check(
            changes=changes, event=self.managed_release_event()
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--skip-release", args)

    def test_managed_release_with_deleted_batch_exempts_missing_entries(self):
        changes = self.release_changes()
        changes[".changenote/releases.json"] = None
        result, args = self.run_note_check(
            changes=changes, event=self.managed_release_event()
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--skip-release", args)

    def test_managed_release_without_batch_exempts_missing_entries(self):
        changes = self.release_changes()
        del changes[".changenote/releases.json"]
        result, args = self.run_note_check(
            changes=changes, event=self.managed_release_event(),
            legacy_batch=False
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--skip-release", args)

    def test_managed_release_empty_diff_keeps_normal_coverage(self):
        result, args = self.run_note_check(
            changes={}, event=self.managed_release_event()
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--skip-release", args)

    def test_managed_release_cli_failure_is_not_hidden(self):
        changes = self.release_changes()
        changes[".changenote/releases.json"] = None
        result, args = self.run_note_check(
            changes=changes, event=self.managed_release_event(), cli_exit=42
        )
        self.assertEqual(result.returncode, 42, result.stderr)
        self.assertIn("--skip-release", args)

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
