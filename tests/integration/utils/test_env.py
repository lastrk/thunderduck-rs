"""Per-worktree test environment: remembered ports + worktree-scoped cleanup.

This is the single source of truth for which ports a worktree's Thunderduck and
Spark-reference servers use during unit/integration/differential testing.

Why this exists
---------------
We develop in multiple git worktrees on one machine, often running tests
concurrently. Ports must not clash across worktrees, must be *remembered* so a
manual PySpark client can reconnect and so cleanup can find dangling servers,
and cleanup must never kill another worktree's (or another dev session's)
servers.

How it works
------------
Ports are picked once (random free ports), persisted to
``<worktree_root>/.thunderduck-test-env.json``, and reused on every subsequent
run. The file is the authoritative record of this worktree's ports. Cleanup is
*ownership-verified*: a listener is only signalled after we prove it belongs to
a Thunderduck test run for the target worktree — the Thunderduck binary runs
from ``<worktree_root>/target/**`` (checked via ``/proc/<pid>/exe``), and the
Spark JVM carries ``-Dthunderduck.worktree=<id>`` on its command line.

Cross-worktree discovery uses ``git worktree list`` — reading each worktree's
env file yields every worktree's ports, so a dangling server can be reaped
without a central registry and without a host-wide ``pkill``.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import random
import signal
import socket
import subprocess
import sys
from pathlib import Path

ENV_FILENAME = ".thunderduck-test-env.json"

# Random port band. Deliberately above the privileged/registered defaults
# (15002/15003) and below the typical Linux ephemeral range (32768+) so a
# remembered port is unlikely to be transiently grabbed by an outbound socket.
PORT_MIN = 15100
PORT_MAX = 18999

# JVM marker placed on the Spark reference process so cleanup can prove which
# worktree a given SparkConnectServer belongs to.
WORKTREE_JVM_PROP = "-Dthunderduck.worktree="



def worktree_root(cwd: str | os.PathLike | None = None) -> Path:
    """Return the git worktree root (``git rev-parse --show-toplevel``).

    Falls back gracefully when git cannot answer — e.g. a relocated worktree
    whose ``.git`` gitfile points at a main repo not present on this host
    (common in devcontainers/CI where the checkout was moved). Behaviour is
    unchanged whenever git works. Otherwise an explicit
    ``THUNDERDUCK_WORKTREE_ROOT`` wins; failing that, the given cwd (or the
    process cwd) is used. Port isolation only needs a stable per-checkout path,
    so any of these is sufficient.
    """
    override = os.environ.get("THUNDERDUCK_WORKTREE_ROOT")
    if override:
        return Path(override).expanduser().resolve()
    try:
        out = subprocess.run(
            ["git", "rev-parse", "--show-toplevel"],
            cwd=str(cwd) if cwd else None,
            capture_output=True, text=True, check=True,
        )
        return Path(out.stdout.strip()).resolve()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return (Path(cwd) if cwd else Path.cwd()).resolve()


def worktree_id(root: Path) -> str:
    """Stable 8-hex-char label derived from the worktree's absolute path."""
    return hashlib.sha256(str(root.resolve()).encode()).hexdigest()[:8]


def _branch(root: Path) -> str:
    try:
        out = subprocess.run(
            ["git", "rev-parse", "--abbrev-ref", "HEAD"],
            cwd=str(root), capture_output=True, text=True, check=True,
        )
        return out.stdout.strip()
    except subprocess.CalledProcessError:
        return "?"


def env_file(root: Path) -> Path:
    return root / ENV_FILENAME



def _is_free(port: int) -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            s.bind(("localhost", port))
            return True
        except OSError:
            return False


def _pick_free_port(exclude: tuple[int, ...] = ()) -> int:
    """Pick a random free port in the band; fall back to OS-assigned."""
    for _ in range(200):
        port = random.randint(PORT_MIN, PORT_MAX)
        if port in exclude:
            continue
        if _is_free(port):
            return port
    # Band exhausted/contended — let the OS assign any free port.
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("localhost", 0))
        return s.getsockname()[1]


def pid_on_port(port: int) -> int | None:
    """PID listening on ``port`` (first match), via lsof; None if none/absent."""
    try:
        out = subprocess.run(
            ["lsof", "-ti", f":{port}"], capture_output=True, text=True,
        )
    except FileNotFoundError:
        return None
    for line in out.stdout.strip().splitlines():
        line = line.strip()
        if line.isdigit():
            return int(line)
    return None


def _pid_exe(pid: int) -> Path | None:
    try:
        return Path(os.readlink(f"/proc/{pid}/exe"))
    except OSError:
        return None


def _pid_cmdline(pid: int) -> str:
    try:
        with open(f"/proc/{pid}/cmdline", "rb") as f:
            return f.read().replace(b"\0", b" ").decode(errors="replace")
    except OSError:
        return ""


def _pid_ppid(pid: int) -> int | None:
    try:
        with open(f"/proc/{pid}/stat") as f:
            # ... field 4 is PPID; comm may contain spaces/parens, so split on ')'.
            after = f.read().rsplit(")", 1)[1].split()
        return int(after[1])
    except (OSError, IndexError, ValueError):
        return None



def owns_listener(port: int, root: Path, wid: str) -> tuple[int | None, str | None]:
    """Classify the listener on ``port`` relative to worktree ``root``.

    Returns ``(pid, kind)`` where ``kind`` is ``"thunderduck"`` / ``"spark"`` if
    the listener provably belongs to this worktree, else ``None`` (found but not
    owned, or nothing listening). Cleanup must only signal when ``kind`` is set.
    """
    pid = pid_on_port(port)
    if pid is None:
        return None, None
    exe = _pid_exe(pid)
    target = str((root / "target").resolve())
    if exe is not None and str(exe.resolve()).startswith(target):
        return pid, "thunderduck"
    cmdline = _pid_cmdline(pid)
    if f"{WORKTREE_JVM_PROP}{wid}" in cmdline:
        return pid, "spark"
    # Fallback: Thunderduck binary path present anywhere on the command line.
    if str((root / "target").resolve()) in cmdline and "thunderduck-connect-server" in cmdline:
        return pid, "thunderduck"
    return pid, None



def read_env(root: Path) -> dict | None:
    path = env_file(root)
    if not path.exists():
        return None
    try:
        data = json.loads(path.read_text())
    except (json.JSONDecodeError, OSError):
        return None
    if not isinstance(data, dict):
        return None
    return data


def write_env(root: Path, td_port: int, spark_port: int) -> dict:
    data = {
        "worktree": str(root),
        "branch": _branch(root),
        "worktree_id": worktree_id(root),
        "thunderduck_port": td_port,
        "spark_port": spark_port,
    }
    env_file(root).write_text(json.dumps(data, indent=2) + "\n")
    return data


def _port_usable(port: int, root: Path, wid: str) -> bool:
    """A remembered port is reusable if free, or held only by our own server.

    (The harness kills our own stale listener before starting a fresh server, so
    a port held by *this* worktree is fine to keep; a port held by a *foreign*
    process must be surrendered.)
    """
    if _is_free(port):
        return True
    _pid, kind = owns_listener(port, root, wid)
    return kind is not None


def resolve_ports(root: Path | None = None) -> tuple[int, int]:
    """Return ``(thunderduck_port, spark_port)`` for this worktree.

    Priority: explicit env vars → remembered file (if still usable) → freshly
    allocated random free ports (persisted). Always (re)writes the env file so
    the record reflects the ports in use.
    """
    if root is None:
        root = worktree_root()
    wid = worktree_id(root)

    env_td = os.environ.get("THUNDERDUCK_PORT")
    env_sp = os.environ.get("SPARK_PORT")
    if env_td and env_sp:
        td, sp = int(env_td), int(env_sp)
        write_env(root, td, sp)
        return td, sp

    data = read_env(root)
    if data:
        td = data.get("thunderduck_port")
        sp = data.get("spark_port")
        if (isinstance(td, int) and isinstance(sp, int) and td != sp
                and _port_usable(td, root, wid) and _port_usable(sp, root, wid)):
            # Re-write to refresh branch/id if the worktree moved.
            write_env(root, td, sp)
            return td, sp

    td = _pick_free_port()
    sp = _pick_free_port(exclude=(td,))
    write_env(root, td, sp)
    return td, sp



def _kill(pid: int) -> None:
    """SIGTERM→SIGKILL the process group of ``pid`` (mirrors the managers)."""
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(os.getpgid(pid), sig)
        except (ProcessLookupError, PermissionError, OSError):
            try:
                os.kill(pid, sig)
            except (ProcessLookupError, PermissionError, OSError):
                return


def _worktree_status(root: Path) -> list[tuple[str, int, int | None, str | None, int | None]]:
    """Return [(name, port, pid, kind, ppid), ...] for this worktree's servers."""
    data = read_env(root)
    if not data:
        return []
    wid = data.get("worktree_id") or worktree_id(root)
    rows = []
    for name, key in (("thunderduck", "thunderduck_port"), ("spark", "spark_port")):
        port = data.get(key)
        if not isinstance(port, int):
            continue
        pid, kind = owns_listener(port, root, wid)
        ppid = _pid_ppid(pid) if pid is not None else None
        rows.append((name, port, pid, kind, ppid))
    return rows


def _all_worktrees() -> list[Path]:
    out = subprocess.run(
        ["git", "worktree", "list", "--porcelain"],
        capture_output=True, text=True, check=True,
    )
    roots = []
    for line in out.stdout.splitlines():
        if line.startswith("worktree "):
            roots.append(Path(line[len("worktree "):].strip()))
    return roots


def kill_worktree(root: Path, stale_only: bool = False) -> int:
    """Kill this worktree's owned servers. Returns count signalled.

    ``stale_only`` restricts to orphaned listeners (PPID==1) — a server whose
    launcher died — so a live test/dev session is never disturbed.
    """
    killed = 0
    for name, port, pid, kind, ppid in _worktree_status(root):
        if pid is None or kind is None:
            continue
        if stale_only and ppid != 1:
            print(f"  skip {name} pid={pid} port={port} (live, ppid={ppid})")
            continue
        print(f"  kill {name} pid={pid} port={port} ({kind})")
        _kill(pid)
        killed += 1
    return killed


def _fmt_status(root: Path) -> str:
    data = read_env(root)
    if not data:
        return f"{root}  (no {ENV_FILENAME})"
    lines = [f"{root}  [{data.get('branch', '?')}]  id={data.get('worktree_id')}"]
    for name, port, pid, kind, ppid in _worktree_status(root):
        if pid is None:
            state = "down"
        elif kind is None:
            state = f"FOREIGN pid={pid} (not ours — will not touch)"
        else:
            state = f"up pid={pid} ppid={ppid} ({kind})"
        lines.append(f"    {name:11} :{port}  {state}")
    return "\n".join(lines)



def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description="Per-worktree test server ports & cleanup")
    g = p.add_mutually_exclusive_group()
    g.add_argument("--export", action="store_true",
                   help="resolve ports and print THUNDERDUCK_PORT/SPARK_PORT for eval")
    g.add_argument("--print-json", action="store_true",
                   help="resolve ports and print the env file JSON")
    g.add_argument("--list", action="store_true", help="show this worktree's servers")
    g.add_argument("--list-all", action="store_true", help="show every worktree's servers")
    g.add_argument("--kill", action="store_true", help="kill this worktree's servers (ownership-verified)")
    g.add_argument("--kill-all", action="store_true", help="kill every worktree's servers (ownership-verified)")
    p.add_argument("--stale", action="store_true",
                   help="with --kill/--kill-all: only reap orphaned (ppid==1) servers")
    args = p.parse_args(argv)

    root = worktree_root()

    if args.export:
        td, sp = resolve_ports(root)
        print(f"THUNDERDUCK_PORT={td}")
        print(f"SPARK_PORT={sp}")
        return 0
    if args.print_json:
        td, sp = resolve_ports(root)
        print(json.dumps(read_env(root), indent=2))
        return 0
    if args.list:
        print(_fmt_status(root))
        return 0
    if args.list_all:
        for wt in _all_worktrees():
            print(_fmt_status(wt))
        return 0
    if args.kill:
        n = kill_worktree(root, stale_only=args.stale)
        print(f"signalled {n} server(s) in {root}")
        return 0
    if args.kill_all:
        total = 0
        for wt in _all_worktrees():
            total += kill_worktree(wt, stale_only=args.stale)
        print(f"signalled {total} server(s) across all worktrees")
        return 0

    # Default: show this worktree's status.
    print(_fmt_status(root))
    return 0


if __name__ == "__main__":
    sys.exit(main())
