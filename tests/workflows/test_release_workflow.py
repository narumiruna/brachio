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

    def test_event_routing_and_manual_default(self):
        events = self.workflow["on"]
        self.assertEqual(set(events), {"pull_request", "push", "workflow_dispatch"})
        self.assertEqual(events["push"], {"branches": ["main"]})
        self.assertEqual(events["pull_request"]["types"],
                         ["opened", "synchronize", "reopened", "labeled", "unlabeled"])
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
        matrix = self.jobs["wheels"]["strategy"]["matrix"]["include"]
        self.assertEqual([entry["target"] for entry in matrix], [
            "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu",
            "x86_64-apple-darwin", "aarch64-apple-darwin",
        ])
        self.assertEqual(self.step("wheels", "Build source distribution")["if"],
                         "matrix.target == 'x86_64-unknown-linux-gnu'")
        for name in ("crates-io", "pypi"):
            self.assertEqual(self.jobs[name]["environment"]["name"], name)

    def test_obsolete_workflows_and_note_pattern(self):
        for name in ("changenote.yml", "publish.yml"):
            self.assertFalse((ROOT / ".github/workflows" / name).exists())
        patterns = json.loads((ROOT / ".changenote/config.json").read_text())["changed_file_patterns"]
        self.assertIn(".github/workflows/release.yml", patterns)
        self.assertNotIn(".github/workflows/publish.yml", patterns)

    def test_pat_guard_fails_without_disclosing_token(self):
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

    def run_note_check(self, base, skip="false"):
        step = self.step("notes", "Check PR notes")
        self.assertNotIn("${{", step["run"])
        with tempfile.TemporaryDirectory() as temporary:
            top = Path(temporary)
            calls = top / "calls"
            uvx = top / "uvx"
            uvx.write_text('#!/usr/bin/env bash\nprintf "%s\\0" "$@" > "$CALLS"\n')
            uvx.chmod(0o755)
            env = dict(os.environ, PATH=str(top) + os.pathsep + os.environ["PATH"],
                       CALLS=str(calls), BASE_BRANCH=base, SKIP_RELEASE=skip)
            result = subprocess.run(["bash", "-e", "-o", "pipefail", "-c", step["run"]],
                                    cwd=top, env=env, text=True, capture_output=True, timeout=10)
            args = calls.read_bytes().decode().split("\0")[:-1] if calls.exists() else []
            self.assertFalse((top / "injected").exists())
            return result, args

    def test_pr_arguments_keep_package_coverage(self):
        result, args = self.run_note_check("main")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(args, ["changenote==0.9.2", "check", "--since=refs/remotes/origin/main", "--coverage=package"])

    def test_pr_exemption_keeps_coverage_and_reason(self):
        result, args = self.run_note_check("main", "true")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--coverage=package", args)
        self.assertEqual(args[-3:], ["--skip-release", "--reason",
                                    "Maintainer approved a note exemption; see the PR description."])

    def test_pr_branch_is_data_not_shell_syntax(self):
        base = "topic/odd; $(touch injected)"
        result, args = self.run_note_check(base)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--since=refs/remotes/origin/" + base, args)

    def test_empty_pr_base_fails_before_cli(self):
        result, args = self.run_note_check("")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("PR base branch is required", result.stderr)
        self.assertEqual(args, [])

if __name__ == "__main__":
    unittest.main()
