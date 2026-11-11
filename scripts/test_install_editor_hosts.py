"""Self-test for scripts/install_editor_hosts.sh.

The script runs against a fake `apt-get`, `sudo` and `sleep` placed first on PATH,
so no network or package state is touched. Each test sets the outcome of the
update, fallback update, download and install calls through environment
variables, then asserts the exit code, the log lines and the apt calls made.
"""

from __future__ import annotations

import shutil
import subprocess
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

SCRIPT = Path(__file__).resolve().parent / "install_editor_hosts.sh"
PACKAGES = ["emacs-nox", "mpv", "vim"]

# Tools the script calls for real. Everything else on the host is absent from the
# fake PATH, so `command -v` only finds the binaries a test creates.
REAL_TOOLS = ["bash", "sed", "mktemp", "grep", "sort", "uniq", "tail", "rm", "mkdir", "timeout", "cat"]

FAKE_APT_GET = r"""#!/bin/bash
printf 'CALL %s\n' "$*" >>"$FAKE_APT_LOG"
verb=
source_list=
source_parts=
for arg in "$@"; do
  case $arg in
    update|install) [[ -n $verb ]] || verb=$arg ;;
    Dir::Etc::SourceList=*) source_list=${arg#Dir::Etc::SourceList=} ;;
    Dir::Etc::SourceParts=*) source_parts=${arg#Dir::Etc::SourceParts=} ;;
  esac
done
if [[ -n $source_list ]]; then
  printf 'SOURCES-BEGIN\n' >>"$FAKE_APT_LOG"
  cat "$source_list" >>"$FAKE_APT_LOG"
  for part in "$source_parts"/*; do
    [[ -f $part ]] && cat "$part" >>"$FAKE_APT_LOG"
  done
  printf 'SOURCES-END\n' >>"$FAKE_APT_LOG"
fi
case $verb in
  update)
    if [[ -n $source_list ]]; then result=$FAKE_FALLBACK_UPDATE; else result=$FAKE_UPDATE; fi
    ;;
  install)
    if [[ " $* " == *" --download-only "* ]]; then
      result=$FAKE_DOWNLOAD
      echo "GET http://azure.archive.ubuntu.com/ubuntu/pool/main/v/vim/vim.deb" >&2
    else
      result=$FAKE_INSTALL
    fi
    ;;
  *) echo "fake apt-get: unexpected arguments: $*" >&2; exit 99 ;;
esac
if [[ $result == ok ]]; then
  exit 0
fi
echo "E: simulated $verb failure" >&2
exit 100
"""

FAKE_SUDO = "#!/bin/bash\nexec \"$@\"\n"
FAKE_SLEEP = "#!/bin/bash\nexit 0\n"
FAKE_BINARY = "#!/bin/bash\nexit 0\n"

RUNNER_SOURCES_LIST = "# runner default\n"
RUNNER_UBUNTU_SOURCES = (
    "Types: deb\n"
    "URIs: mirror+file:/etc/apt/apt-mirrors.txt\n"
    "Suites: noble noble-updates\n"
    "Components: main universe\n"
)
RUNNER_LEGACY_SOURCES = "deb http://azure.archive.ubuntu.com/ubuntu noble main\n"


class InstallEditorHostsTests(unittest.TestCase):
    def setUp(self) -> None:
        tmp = TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.bin = self.root / "bin"
        self.apt_root = self.root / "apt"
        self.tmpdir = self.root / "tmp"
        self.bin.mkdir()
        self.tmpdir.mkdir()
        (self.apt_root / "sources.list.d").mkdir(parents=True)
        (self.apt_root / "sources.list").write_text(RUNNER_SOURCES_LIST, encoding="utf-8")
        (self.apt_root / "sources.list.d" / "ubuntu.sources").write_text(RUNNER_UBUNTU_SOURCES, encoding="utf-8")
        (self.apt_root / "sources.list.d" / "legacy.list").write_text(RUNNER_LEGACY_SOURCES, encoding="utf-8")
        self.apt_log = self.root / "apt.log"

        for tool in REAL_TOOLS:
            real = shutil.which(tool)
            self.assertIsNotNone(real, tool)
            (self.bin / tool).symlink_to(real)
        self._write_exec("apt-get", FAKE_APT_GET)
        self._write_exec("sudo", FAKE_SUDO)
        self._write_exec("sleep", FAKE_SLEEP)

    def _write_exec(self, name: str, body: str) -> None:
        path = self.bin / name
        path.write_text(body, encoding="utf-8")
        path.chmod(0o755)

    def _install_binaries(self, *names: str) -> None:
        for name in names:
            self._write_exec(name, FAKE_BINARY)

    def run_script(
        self,
        *,
        update: str,
        fallback_update: str = "ok",
        download: str = "ok",
        install: str = "ok",
        binaries: tuple[str, ...] = ("emacs", "mpv", "vim"),
    ) -> subprocess.CompletedProcess[str]:
        self._install_binaries(*binaries)
        env = {
            "PATH": str(self.bin),
            "TMPDIR": str(self.tmpdir),
            "HOME": str(self.root),
            "FAKE_APT_LOG": str(self.apt_log),
            "FAKE_UPDATE": update,
            "FAKE_FALLBACK_UPDATE": fallback_update,
            "FAKE_DOWNLOAD": download,
            "FAKE_INSTALL": install,
            "INSTALL_EDITOR_APT_ROOT": str(self.apt_root),
        }
        return subprocess.run(
            ["bash", str(SCRIPT), *PACKAGES],
            env=env,
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )

    def apt_calls(self) -> list[str]:
        if not self.apt_log.exists():
            return []
        return [line.removeprefix("CALL ") for line in self.apt_log.read_text(encoding="utf-8").splitlines() if line.startswith("CALL ")]

    def updates(self) -> list[str]:
        return [call for call in self.apt_calls() if call.split()[-1] == "update"]

    def installs(self) -> list[str]:
        return [call for call in self.apt_calls() if " install -y --no-install-recommends " in f" {call} "]

    def fallback_updates(self) -> list[str]:
        return [call for call in self.updates() if "Dir::Etc::SourceList=" in call]

    @staticmethod
    def log_text(result: subprocess.CompletedProcess[str]) -> str:
        return result.stdout + result.stderr

    def test_success_needs_no_fallback(self) -> None:
        result = self.run_script(update="ok")
        log = self.log_text(result)
        self.assertEqual(result.returncode, 0, log)
        self.assertIn("apt-get update succeeded on the runner's default archive", log)
        self.assertIn("Verified binaries on PATH for: emacs-nox mpv vim", log)
        self.assertNotIn("::warning::", log)
        self.assertNotIn("::error::", log)
        self.assertEqual(self.fallback_updates(), [])

    def test_update_failure_then_install_success_does_not_exit(self) -> None:
        result = self.run_script(update="fail", fallback_update="fail", download="fail", install="ok")
        log = self.log_text(result)
        self.assertEqual(result.returncode, 0, log)
        self.assertIn("::warning::Attempt 1 of 2 failed", log)
        self.assertIn(
            "::warning::apt-get update failed after 2 attempts on the runner's default archive; trying archive.ubuntu.com once",
            log,
        )
        self.assertIn(
            "::warning::apt-get update failed on archive.ubuntu.com as well; installing from the package lists already on the runner",
            log,
        )
        self.assertIn("::warning::Download failed after 3 attempts", log)
        self.assertIn("Verified binaries on PATH for: emacs-nox mpv vim", log)
        self.assertNotIn("::error::", log)
        self.assertEqual(len(self.updates()), 3, self.apt_calls())
        self.assertEqual(len(self.installs()), 1, self.apt_calls())
        self.assertNotIn("Dir::Etc::SourceList", self.installs()[0])

    def test_update_failure_then_install_failure_exits_nonzero(self) -> None:
        result = self.run_script(update="fail", fallback_update="fail", download="fail", install="fail")
        log = self.log_text(result)
        self.assertEqual(result.returncode, 1, log)
        self.assertIn("::error::apt-get install failed for: emacs-nox mpv vim (mirror: primary)", log)
        self.assertNotIn("Verified binaries", log)

    def test_missing_binary_after_install_names_the_package(self) -> None:
        result = self.run_script(update="fail", fallback_update="fail", install="ok", binaries=("emacs", "vim"))
        log = self.log_text(result)
        self.assertEqual(result.returncode, 1, log)
        self.assertIn("::error::Required binary 'mpv' (package mpv) is not on PATH after install", log)
        self.assertNotIn("Verified binaries", log)

    def test_fallback_mirror_rescues_failed_update(self) -> None:
        result = self.run_script(update="fail", fallback_update="ok", download="ok", install="ok")
        log = self.log_text(result)
        self.assertEqual(result.returncode, 0, log)
        self.assertIn("apt-get update succeeded on archive.ubuntu.com", log)
        self.assertNotIn("installing from the package lists already on the runner", log)
        self.assertEqual(len(self.fallback_updates()), 1, self.apt_calls())
        self.assertEqual(len(self.installs()), 1, self.apt_calls())
        self.assertIn("Dir::Etc::SourceList=", self.installs()[0])

        apt_log = self.apt_log.read_text(encoding="utf-8")
        rewritten = apt_log.split("SOURCES-BEGIN\n", 1)[1].split("SOURCES-END\n", 1)[0]
        self.assertIn("URIs: http://archive.ubuntu.com/ubuntu/", rewritten)
        self.assertIn("deb http://archive.ubuntu.com/ubuntu noble main", rewritten)
        self.assertNotIn("azure.archive.ubuntu.com", rewritten)
        self.assertNotIn("mirror+file:/etc/apt/apt-mirrors.txt", rewritten)

        # The runner's own sources are never edited; the fallback works on copies.
        self.assertEqual(
            (self.apt_root / "sources.list.d" / "ubuntu.sources").read_text(encoding="utf-8"),
            RUNNER_UBUNTU_SOURCES,
        )
        self.assertEqual(
            (self.apt_root / "sources.list.d" / "legacy.list").read_text(encoding="utf-8"),
            RUNNER_LEGACY_SOURCES,
        )

    def test_failed_download_switches_to_fallback_once_and_install_still_runs(self) -> None:
        result = self.run_script(update="ok", fallback_update="ok", download="fail", install="ok")
        log = self.log_text(result)
        self.assertEqual(result.returncode, 0, log)
        self.assertIn("::warning::Download attempt 1 of 3 failed", log)
        self.assertIn("::warning::Download attempt 1 failed; switching to the Ubuntu archive and refreshing lists", log)
        self.assertIn("::warning::Download failed after 3 attempts; the install step will fetch what is still missing", log)
        self.assertEqual(len(self.fallback_updates()), 1, self.apt_calls())
        self.assertIn("Verified binaries on PATH for: emacs-nox mpv vim", log)

    def test_usage_without_packages_exits_two(self) -> None:
        result = subprocess.run(
            ["bash", str(SCRIPT)],
            env={"PATH": str(self.bin)},
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("usage:", result.stderr)


if __name__ == "__main__":
    unittest.main()
