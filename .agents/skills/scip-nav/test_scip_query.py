import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("scip_query.py")


class ScipQueryWorktreeTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.repository = self.root / "repository"
        self.worktree = self.root / "linked-worktree"
        self.fake_bin = self.root / "bin"
        self.fake_log = self.root / "rust-analyzer.log"

        self._run(["git", "init", "-q", str(self.repository)])
        self._git("config", "user.email", "scip-nav@example.invalid")
        self._git("config", "user.name", "scip-nav test")
        source = self.repository / "src" / "lib.rs"
        source.parent.mkdir()
        source.write_text("pub fn original() {}\n", encoding="utf-8")
        self._git("add", ".")
        self._git("commit", "-q", "-m", "initial")
        self._git("worktree", "add", "-q", "-b", "linked-state", str(self.worktree))

        self.fake_bin.mkdir()
        rust_analyzer = self.fake_bin / "rust-analyzer"
        rust_analyzer.write_text(
            """#!/usr/bin/env python3
import os
from pathlib import Path
import sys
import time

if sys.argv[1:] == ["--version"]:
    print("rust-analyzer test-version")
    raise SystemExit(0)

workspace = sys.argv[2]
output = sys.argv[sys.argv.index("--output") + 1]
with open(os.environ["SCIP_FAKE_LOG"], "a", encoding="utf-8") as log:
    log.write(workspace + "\\n")
if mutation := os.environ.get("SCIP_FAKE_MUTATE"):
    Path(mutation).write_text("pub fn changed_during_indexing() {}\\n", encoding="utf-8")
time.sleep(0.15)
Path(output).write_text(workspace, encoding="utf-8")
""",
            encoding="utf-8",
        )
        rust_analyzer.chmod(0o755)

        self.environment = os.environ.copy()
        self.environment.pop("SCIP_WORKSPACE", None)
        self.environment.pop("SCIP_CACHE_ROOT", None)
        self.environment["PATH"] = (
            str(self.fake_bin) + os.pathsep + self.environment["PATH"]
        )
        self.environment["SCIP_FAKE_LOG"] = str(self.fake_log)

    def tearDown(self):
        self.temporary.cleanup()

    def _run(self, command, **kwargs):
        return subprocess.run(
            command, check=True, text=True, capture_output=True, **kwargs
        )

    def _git(self, *args):
        return self._run(["git", "-C", str(self.repository), *args])

    def _scip(self, cwd, *args, check=True, environment=None):
        return subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            cwd=cwd,
            env=environment or self.environment,
            check=check,
            text=True,
            capture_output=True,
        )

    def _status(self, cwd, environment=None):
        output = self._scip(cwd, "status", environment=environment).stdout
        return {
            key: value
            for line in output.splitlines()
            if ": " in line
            for key, value in [line.split(": ", 1)]
        }

    def test_workspace_and_cache_are_separate_and_content_keyed(self):
        repository = self._status(self.repository)
        linked = self._status(self.worktree)

        self.assertEqual(repository["workspace"], str(self.repository))
        self.assertEqual(linked["workspace"], str(self.worktree))
        self.assertEqual(repository["cache"], str(self.repository / ".scip"))
        self.assertEqual(linked["cache"], repository["cache"])
        self.assertEqual(linked["fingerprint"], repository["fingerprint"])
        self.assertEqual(linked["snapshot"], repository["snapshot"])

        untracked = self.worktree / "same-size.rs"
        untracked.write_text("aa", encoding="utf-8")
        initial_mtime = untracked.stat().st_mtime_ns
        first = self._status(self.worktree)["fingerprint"]
        untracked.write_text("bb", encoding="utf-8")
        os.utime(untracked, ns=(initial_mtime, initial_mtime))
        second = self._status(self.worktree)["fingerprint"]

        self.assertNotEqual(first, second)
        self.assertEqual(
            self._status(self.repository)["fingerprint"], repository["fingerprint"]
        )

    def test_workspace_and_cache_overrides_are_independent(self):
        environment = self.environment.copy()
        custom_cache = self.root / "custom-cache"
        environment["SCIP_WORKSPACE"] = str(self.worktree)
        environment["SCIP_CACHE_ROOT"] = str(custom_cache)

        status = self._status(self.repository, environment=environment)

        self.assertEqual(status["workspace"], str(self.worktree))
        self.assertEqual(status["cache"], str(custom_cache))

    def test_stale_snapshot_pointer_is_scoped_to_one_worktree(self):
        linked = self._status(self.worktree)
        index = Path(linked["snapshot"])
        index.parent.mkdir(parents=True)
        index.write_bytes(b"")
        self._scip(self.worktree, "def", "Absent")

        (self.worktree / "src" / "lib.rs").write_text(
            "pub fn linked_dirty() {}\n", encoding="utf-8"
        )
        stale = self._scip(self.worktree, "--stale-ok", "def", "Absent")
        self.assertIn("this worktree's previous snapshot", stale.stderr)

        (self.repository / "src" / "lib.rs").write_text(
            "pub fn repository_dirty() {}\n", encoding="utf-8"
        )
        rejected = self._scip(
            self.repository, "--stale-ok", "def", "Absent", check=False
        )
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("No previous snapshot exists for this worktree", rejected.stderr)

    def test_refresh_discards_snapshot_if_source_changes_during_indexing(self):
        status = self._status(self.repository)
        environment = self.environment.copy()
        environment["SCIP_FAKE_MUTATE"] = str(self.repository / "src" / "lib.rs")

        refresh = self._scip(
            self.repository, "refresh", check=False, environment=environment
        )

        self.assertNotEqual(refresh.returncode, 0)
        self.assertIn("source changed while rust-analyzer was indexing", refresh.stderr)
        self.assertFalse(Path(status["snapshot"]).exists())

    def test_concurrent_refresh_reuses_exact_state_and_isolates_dirty_states(self):
        clean = [
            subprocess.Popen(
                [sys.executable, str(SCRIPT), "refresh"],
                cwd=path,
                env=self.environment,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            for path in (self.repository, self.worktree)
        ]
        for process in clean:
            process.communicate(timeout=10)
            self.assertEqual(process.returncode, 0)

        clean_repository = self._status(self.repository)
        clean_linked = self._status(self.worktree)
        self.assertEqual(clean_repository["snapshot"], clean_linked["snapshot"])
        self.assertEqual(len(self.fake_log.read_text(encoding="utf-8").splitlines()), 1)

        (self.repository / "src" / "lib.rs").write_text("pub fn left() {}\n")
        (self.worktree / "src" / "lib.rs").write_text("pub fn right() {}\n")
        self.fake_log.write_text("", encoding="utf-8")
        dirty = [
            subprocess.Popen(
                [sys.executable, str(SCRIPT), "refresh"],
                cwd=path,
                env=self.environment,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            for path in (self.repository, self.worktree)
        ]
        for process in dirty:
            process.communicate(timeout=10)
            self.assertEqual(process.returncode, 0)

        dirty_repository = self._status(self.repository)
        dirty_linked = self._status(self.worktree)
        self.assertNotEqual(dirty_repository["snapshot"], dirty_linked["snapshot"])
        self.assertEqual(len(self.fake_log.read_text(encoding="utf-8").splitlines()), 2)
        self.assertEqual(
            Path(dirty_repository["snapshot"]).read_text(encoding="utf-8"),
            str(self.repository),
        )
        self.assertEqual(
            Path(dirty_linked["snapshot"]).read_text(encoding="utf-8"),
            str(self.worktree),
        )


if __name__ == "__main__":
    unittest.main()
