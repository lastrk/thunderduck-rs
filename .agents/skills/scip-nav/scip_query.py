#!/usr/bin/env python3
"""scip-nav: query a rust-analyzer SCIP snapshot for type-accurate code navigation.

Provides trait-/type-resolved references, exact definitions, and workspace symbol
search from static, content-keyed snapshots with ZERO resident language server.

Pure-Python SCIP protobuf reader: no protoc, no scip CLI, no dependencies.
SCIP schema field numbers per github.com/sourcegraph/scip/blob/main/scip.proto.

FRESHNESS IS STRUCTURAL: every query verifies the index against the current working
tree via a fast git fingerprint and FAILS CLOSED on a stale index (refuses to return
possibly-wrong results) unless you pass --stale-ok or --auto-refresh.

Usage:
    scip_query.py refs <name>      # all references (call sites) of a symbol
    scip_query.py def  <name>      # definition(s) of a symbol
    scip_query.py sym  <name>      # workspace symbol search (fuzzy substring)
    scip_query.py expand <crate> [pat]  # macro-EXPANDED source (nightly rustc; compile-bound,
                                        # crate-scoped, needs a green tree); slice by regex `pat`
    scip_query.py refresh          # generate or reuse the exact immutable snapshot
    scip_query.py status           # workspace, cache, fingerprint, and snapshot

Flags: --count (terse integer), --stale-ok (serve this worktree's previous
       snapshot w/ warning), --auto-refresh (generate the exact snapshot).
Env: SCIP_WORKSPACE (default: current Git worktree root),
     SCIP_CACHE_ROOT (default: <git-common-dir>/../.scip).
"""

import fcntl
import hashlib
import os
import re
import resource
import stat
import subprocess
import sys
import time

CACHE_SCHEMA = b"scip-nav-snapshot-v2"


def _git_at(root, *args, text=True):
    return subprocess.check_output(
        ["git", "-C", root, *args],
        text=text,
        stderr=subprocess.DEVNULL,
    )


def _discover_workspace():
    candidate = os.path.realpath(os.environ.get("SCIP_WORKSPACE", os.getcwd()))
    try:
        return os.path.realpath(
            _git_at(candidate, "rev-parse", "--show-toplevel").strip()
        )
    except Exception:
        return candidate


def _discover_cache_root(workspace):
    override = os.environ.get("SCIP_CACHE_ROOT")
    if override:
        return os.path.realpath(override)
    try:
        common = _git_at(
            workspace,
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ).strip()
        if not os.path.isabs(common):
            common = os.path.join(workspace, common)
        return os.path.join(os.path.dirname(os.path.realpath(common)), ".scip")
    except Exception:
        return os.path.join(workspace, ".scip")


WS = _discover_workspace()
CACHE_ROOT = _discover_cache_root(WS)
SNAPSHOT_ROOT = os.path.join(CACHE_ROOT, "snapshots")
LOCK_ROOT = os.path.join(CACHE_ROOT, "locks")
TMP_ROOT = os.path.join(CACHE_ROOT, "tmp")
WORKTREE_ROOT = os.path.join(CACHE_ROOT, "worktrees")
WORKTREE_KEY = hashlib.sha256(WS.encode()).hexdigest()[:20]
LAST_FP_PATH = os.path.join(WORKTREE_ROOT, WORKTREE_KEY, "last-fingerprint")

DEFINITION = 0x1  # SymbolRole bit

# Friendly crate aliases for `expand` (maps to Cargo package names).
CRATE_ALIASES = {
    "core": "thunderduck-core",
    "connect-server": "thunderduck-connect-server",
    "server": "thunderduck-connect-server",
}

# ---------- minimal protobuf wire decoder ----------


def _varint(buf, i):
    shift = result = 0
    while True:
        b = buf[i]
        i += 1
        result |= (b & 0x7F) << shift
        if not (b & 0x80):
            return result, i
        shift += 7


def _fields(buf):
    """Yield (field_number, wire_type, value) over one message."""
    i, n = 0, len(buf)
    while i < n:
        tag, i = _varint(buf, i)
        fn, wt = tag >> 3, tag & 7
        if wt == 0:
            v, i = _varint(buf, i)
            yield fn, wt, v
        elif wt == 2:
            ln, i = _varint(buf, i)
            yield fn, wt, buf[i : i + ln]
            i += ln
        elif wt == 1:
            yield fn, wt, buf[i : i + 8]
            i += 8
        elif wt == 5:
            yield fn, wt, buf[i : i + 4]
            i += 4
        else:
            raise ValueError(f"unsupported wire type {wt}")


def _packed_varints(buf):
    i, n = 0, len(buf)
    while i < n:
        v, i = _varint(buf, i)
        yield v


# ---------- SCIP structure ----------


def _parse_occurrence(buf):
    symbol, roles, rng = None, 0, []
    for fn, wt, val in _fields(buf):
        if fn == 1:
            rng = list(_packed_varints(val)) if wt == 2 else (rng + [val])
        elif fn == 2 and wt == 2:
            symbol = val.decode("utf-8", "replace")
        elif fn == 3 and wt == 0:
            roles = val
        elif fn in (8, 9) and wt == 2 and not rng:
            rng = list(_packed_varints(val))
    return symbol, roles, rng


def _parse_document(buf):
    path, occs = None, []
    for fn, wt, val in _fields(buf):
        if fn == 1 and wt == 2:
            path = val.decode("utf-8", "replace")
        elif fn == 2 and wt == 2:
            occs.append(_parse_occurrence(val))
    return path, occs


def load_documents(path):
    if not os.path.exists(path):
        sys.exit(f"error: no SCIP index at {path} — run `scip_query.py refresh` first")
    with open(path, "rb") as f:
        data = f.read()
    docs = []
    for fn, wt, val in _fields(data):
        if fn == 2 and wt == 2:
            docs.append(_parse_document(val))
    return docs


# ---------- worktree state and immutable snapshots ----------


def _git(*args, text=True):
    return _git_at(WS, *args, text=text)


def _rust_analyzer_version():
    try:
        return subprocess.check_output(
            ["rust-analyzer", "--version"],
            text=True,
            stderr=subprocess.DEVNULL,
            timeout=5,
        ).strip()
    except (OSError, subprocess.SubprocessError):
        return "rust-analyzer-unknown"


RUST_ANALYZER_VERSION = _rust_analyzer_version()


def _hash_part(digest, label, value):
    digest.update(label)
    digest.update(len(value).to_bytes(8, "big"))
    digest.update(value)


def _hash_untracked_file(digest, raw_path):
    path = os.path.join(WS, os.fsdecode(raw_path))
    _hash_part(digest, b"path", raw_path)
    try:
        metadata = os.lstat(path)
        _hash_part(digest, b"mode", str(metadata.st_mode).encode())
        if stat.S_ISLNK(metadata.st_mode):
            _hash_part(digest, b"link", os.fsencode(os.readlink(path)))
        elif stat.S_ISREG(metadata.st_mode):
            content = hashlib.sha256()
            with open(path, "rb") as source:
                for chunk in iter(lambda: source.read(1024 * 1024), b""):
                    content.update(chunk)
            _hash_part(digest, b"file", content.digest())
        else:
            _hash_part(digest, b"special", b"")
    except OSError:
        _hash_part(digest, b"missing", b"")


def _inside_cache(raw_path):
    candidate = os.path.abspath(os.path.join(WS, os.fsdecode(raw_path)))
    for root in (SNAPSHOT_ROOT, LOCK_ROOT, TMP_ROOT, WORKTREE_ROOT):
        try:
            if os.path.commonpath((candidate, root)) == root:
                return True
        except ValueError:
            pass
    return False


def source_fingerprint():
    """Return a content fingerprint for the current Git worktree state."""
    try:
        head = _git("rev-parse", "HEAD").strip()
        tracked = _git("diff", "--no-ext-diff", "--binary", "HEAD", "--", text=False)
        untracked = _git("ls-files", "--others", "--exclude-standard", "-z", text=False)
    except (OSError, subprocess.SubprocessError):
        return None

    digest = hashlib.sha256()
    _hash_part(digest, b"schema", CACHE_SCHEMA)
    _hash_part(digest, b"rust-analyzer", RUST_ANALYZER_VERSION.encode())
    _hash_part(digest, b"head", head.encode())
    _hash_part(digest, b"tracked", tracked)
    for raw_path in sorted(filter(None, untracked.split(b"\0"))):
        if _inside_cache(raw_path):
            continue
        _hash_untracked_file(digest, raw_path)
    return f"{head[:12]}-{digest.hexdigest()}"


def _snapshot_dir(fingerprint):
    return os.path.join(SNAPSHOT_ROOT, fingerprint)


def _index_path(fingerprint):
    return os.path.join(_snapshot_dir(fingerprint), "index.scip")


def _read(path):
    try:
        with open(path, encoding="utf-8") as source:
            return source.read().strip()
    except OSError:
        return None


def _atomic_write(path, value):
    directory = os.path.dirname(path)
    os.makedirs(directory, exist_ok=True)
    temporary = os.path.join(
        directory, f".{os.path.basename(path)}.{os.getpid()}.{time.time_ns()}.tmp"
    )
    try:
        with open(temporary, "w", encoding="utf-8") as target:
            target.write(value)
            target.flush()
            os.fsync(target.fileno())
        os.replace(temporary, path)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def _record_last_snapshot(fingerprint):
    _atomic_write(LAST_FP_PATH, fingerprint)


def _last_snapshot():
    fingerprint = _read(LAST_FP_PATH)
    if not fingerprint:
        return None, None
    index = _index_path(fingerprint)
    if not os.path.isfile(index):
        return None, None
    return fingerprint, index


def ensure_fresh(auto_refresh=False, stale_ok=False):
    """Return an exact index, or this worktree's prior index when explicitly allowed."""
    fingerprint = source_fingerprint()
    if fingerprint is None:
        sys.exit(f"scip-nav: ERROR — {WS} is not an indexed Git worktree")

    index = _index_path(fingerprint)
    if os.path.isfile(index):
        _record_last_snapshot(fingerprint)
        return index
    if auto_refresh:
        sys.stderr.write("scip-nav: exact snapshot missing — auto-refreshing...\n")
        return cmd_refresh()
    if stale_ok:
        previous_fingerprint, previous_index = _last_snapshot()
        if previous_index:
            sys.stderr.write(
                "scip-nav: WARNING — serving this worktree's previous snapshot "
                f"({previous_fingerprint}); --stale-ok set.\n"
            )
            return previous_index

    relative_script = os.path.relpath(__file__, WS)
    suffix = (
        " No previous snapshot exists for this worktree, so --stale-ok cannot help."
        if stale_ok
        else ""
    )
    sys.exit(
        "scip-nav: ERROR — no exact SCIP snapshot for the current worktree state."
        f"{suffix}\n"
        f"  workspace: {WS}\n"
        f"  expected:  {index}\n"
        f"  fix:       python3 {relative_script} refresh\n"
        "  or:        re-run with --auto-refresh"
    )


# ---------- matching ----------


def _matches(symbol, name):
    if not symbol:
        return False
    return (
        re.search(
            r"(?:^|[^A-Za-z0-9_])" + re.escape(name) + r"(?:[^A-Za-z0-9_]|$)", symbol
        )
        is not None
    )


def _line(rng):
    return (rng[0] + 1) if rng else 0


# ---------- commands ----------


def cmd_refs(name, index, count_only=False):
    hits = {}
    for path, occs in load_documents(index):
        for symbol, roles, rng in occs:
            if _matches(symbol, name) and not (roles & DEFINITION):
                hits.setdefault(path, []).append(_line(rng))
    total = sum(len(v) for v in hits.values())
    if count_only:
        print(total)
        return
    print(f"references to '{name}': {total} across {len(hits)} file(s)\n")
    for path in sorted(hits, key=lambda p: -len(hits[p])):
        lines = sorted(hits[path])
        preview = ", ".join(map(str, lines[:12]))
        more = "" if len(lines) <= 12 else f", +{len(lines)-12} more"
        print(f"  {path}  ({len(lines)})\n    lines: {preview}{more}")


def cmd_def(name, index, count_only=False):
    defs = []
    for path, occs in load_documents(index):
        for symbol, roles, rng in occs:
            if _matches(symbol, name) and (roles & DEFINITION):
                defs.append((path, _line(rng), symbol))
    if count_only:
        print(len(defs))
        return
    print(f"definition(s) of '{name}': {len(defs)}\n")
    for path, line, symbol in sorted(defs):
        print(f"  {path}:{line}\n    {symbol}")


def cmd_sym(name, index, count_only=False):
    syms = {}
    for path, occs in load_documents(index):
        for symbol, roles, rng in occs:
            if (roles & DEFINITION) and symbol and name.lower() in symbol.lower():
                syms.setdefault(symbol, (path, _line(rng)))
    if count_only:
        print(len(syms))
        return
    print(f"symbols matching '{name}': {len(syms)}\n")
    for symbol, (path, line) in sorted(syms.items(), key=lambda kv: kv[1]):
        desc = symbol.split(" ")[-1] if " " in symbol else symbol
        print(f"  {path}:{line}  {desc}")


def cmd_status():
    fingerprint = source_fingerprint()
    print(f"workspace: {WS}")
    print(f"cache: {CACHE_ROOT}")
    print(f"rust-analyzer: {RUST_ANALYZER_VERSION}")
    if fingerprint is None:
        print("fingerprint: UNAVAILABLE (workspace is not a Git worktree)")
        print("snapshot: MISSING")
        return

    index = _index_path(fingerprint)
    print(f"fingerprint: {fingerprint}")
    print(f"snapshot: {index}")
    if os.path.isfile(index):
        size = os.path.getsize(index) / (1024 * 1024)
        built = time.ctime(os.path.getmtime(index))
        print(f"index: EXACT ({size:.1f} MB, built {built})")
        return

    print("index: MISSING — run `scip_query.py refresh`")
    previous_fingerprint, previous_index = _last_snapshot()
    if previous_index:
        print(
            "previous-for-worktree: "
            f"{previous_index} ({previous_fingerprint}; available via --stale-ok)"
        )


def _limit_address_space():
    limit = 7_500_000 * 1024
    _, hard = resource.getrlimit(resource.RLIMIT_AS)
    if hard != resource.RLIM_INFINITY:
        limit = min(limit, hard)
    resource.setrlimit(resource.RLIMIT_AS, (limit, hard))


def _temporary_path(name):
    os.makedirs(TMP_ROOT, exist_ok=True)
    return os.path.join(TMP_ROOT, f"{name}.{os.getpid()}.{time.time_ns()}.tmp")


def cmd_refresh():
    fingerprint = source_fingerprint()
    if fingerprint is None:
        sys.exit(f"scip-nav: ERROR — cannot fingerprint non-Git workspace {WS}")

    os.makedirs(LOCK_ROOT, exist_ok=True)
    lock_path = os.path.join(LOCK_ROOT, f"index-{fingerprint}.lock")
    index = _index_path(fingerprint)
    with open(lock_path, "w", encoding="utf-8") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if source_fingerprint() != fingerprint:
            sys.exit(
                "scip-nav: source changed while waiting for the refresh lock; retry"
            )
        if os.path.isfile(index):
            _record_last_snapshot(fingerprint)
            print(f"reused: {index}")
            return index

        os.makedirs(_snapshot_dir(fingerprint), exist_ok=True)
        temporary = _temporary_path(f"index-{fingerprint}")
        analyzer_log = _temporary_path(f"index-{fingerprint}-rust-analyzer")
        print(
            f"generating SCIP index for {WS} "
            "(rust-analyzer scip — ~15s, ~3GB transient)...",
            flush=True,
        )
        try:
            try:
                with open(analyzer_log, "wb") as output:
                    rc = subprocess.call(
                        [
                            "nice",
                            "-n",
                            "15",
                            "timeout",
                            "400",
                            "rust-analyzer",
                            "scip",
                            WS,
                            "--output",
                            temporary,
                        ],
                        cwd=WS,
                        stdout=output,
                        stderr=subprocess.STDOUT,
                        preexec_fn=_limit_address_space,
                    )
            except OSError as error:
                sys.exit(f"scip-nav: rust-analyzer scip could not start: {error}")
            if (
                rc != 0
                or not os.path.isfile(temporary)
                or os.path.getsize(temporary) == 0
            ):
                sys.exit(
                    f"scip-nav: rust-analyzer scip failed (rc={rc}). Output tail:\n"
                    f"{_tail(analyzer_log, 30)}"
                )
            if source_fingerprint() != fingerprint:
                sys.exit(
                    "scip-nav: source changed while rust-analyzer was indexing; "
                    "discarded the temporary snapshot"
                )
            os.replace(temporary, index)
        finally:
            for path in (temporary, analyzer_log):
                try:
                    os.unlink(path)
                except FileNotFoundError:
                    pass

        _record_last_snapshot(fingerprint)
        size = os.path.getsize(index) / (1024 * 1024)
        print(f"ok: {index} ({size:.1f} MB)")
        return index


def _show_region(path, pattern, before=2, after=24, max_hits=6):
    rx = re.compile(pattern)
    lines = open(path, encoding="utf-8", errors="replace").read().splitlines()
    hits = [i for i, l in enumerate(lines) if rx.search(l)]
    if not hits:
        print(f"no match for /{pattern}/ in {path} (try a broader pattern)")
        return
    print(f"{len(hits)} match(es) for /{pattern}/ (showing up to {max_hits}):\n")
    for i in hits[:max_hits]:
        lo, hi = max(0, i - before), min(len(lines), i + after + 1)
        print(f"--- {path}:{i+1} ---")
        for n in range(lo, hi):
            print(f"{'>>' if n == i else '  '}{n+1:6} {lines[n]}")
        print()
    if len(hits) > max_hits:
        print(f"... +{len(hits)-max_hits} more matches; refine the pattern.")


def cmd_expand(crate, pattern=None):
    """Macro-EXPANDED source via nightly `rustc -Zunpretty=expanded`. Compile-bound,
    crate-scoped, needs a green tree. Shows:
    the actual code generated by macro_rules!/derive/proc-macros."""
    pkg = CRATE_ALIASES.get(crate, crate)
    fingerprint = source_fingerprint()
    if fingerprint is None:
        sys.exit(f"scip-nav: ERROR — cannot fingerprint non-Git workspace {WS}")

    package_key = (
        re.sub(r"[^A-Za-z0-9_.-]", "_", pkg)
        + "-"
        + hashlib.sha256(pkg.encode()).hexdigest()[:12]
    )
    out = os.path.join(_snapshot_dir(fingerprint), f"expanded-{package_key}.rs")
    if not os.path.isfile(out):
        os.makedirs(LOCK_ROOT, exist_ok=True)
        lock_path = os.path.join(
            LOCK_ROOT,
            f"expand-{fingerprint}-{hashlib.sha256(pkg.encode()).hexdigest()}.lock",
        )
        with open(lock_path, "w", encoding="utf-8") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            if source_fingerprint() != fingerprint:
                sys.exit(
                    "scip-nav: source changed while waiting for the expand lock; retry"
                )
            if not os.path.isfile(out):
                temporary = _temporary_path(f"expanded-{package_key}")
                error_log = _temporary_path(f"expanded-{package_key}-stderr")
                sys.stderr.write(
                    f"scip-nav: expanding {pkg} via nightly rustc "
                    "(compile-bound ~cargo-check cost; needs a green tree)...\n"
                )
                try:
                    with open(temporary, "wb") as expanded, open(
                        error_log, "wb"
                    ) as errors:
                        try:
                            rc = subprocess.call(
                                [
                                    "nice",
                                    "-n",
                                    "15",
                                    "timeout",
                                    "600",
                                    "cargo",
                                    "+nightly",
                                    "rustc",
                                    "-p",
                                    pkg,
                                    "--lib",
                                    "--",
                                    "-Zunpretty=expanded",
                                ],
                                stdout=expanded,
                                stderr=errors,
                                cwd=WS,
                            )
                        except OSError as error:
                            sys.exit(f"scip-nav: expand could not start: {error}")
                    if rc != 0 or os.path.getsize(temporary) == 0:
                        sys.exit(
                            f"scip-nav: expand FAILED (rc={rc}) — tree not green, "
                            "nightly missing, or cargo lock held by a concurrent build. "
                            f"stderr tail:\n{_tail(error_log, 20)}"
                        )
                    if source_fingerprint() != fingerprint:
                        sys.exit(
                            "scip-nav: source changed during macro expansion; "
                            "discarded the temporary output"
                        )
                    os.makedirs(_snapshot_dir(fingerprint), exist_ok=True)
                    os.replace(temporary, out)
                finally:
                    for path in (temporary, error_log):
                        try:
                            os.unlink(path)
                        except FileNotFoundError:
                            pass
                sys.stderr.write(
                    f"scip-nav: cached {out} ({os.path.getsize(out) // 1024} KB)\n"
                )
    if not pattern:
        n = sum(1 for _ in open(out, encoding="utf-8", errors="replace"))
        print(
            f"expanded {pkg}: {out} ({n} lines). Slice it, e.g. "
            f"`expand {crate} 'fn children'` or `expand {crate} 'bail_boundary'`."
        )
        return
    _show_region(out, pattern)


def _tail(p, n):
    try:
        return "\n".join(
            open(p, encoding="utf-8", errors="replace").read().splitlines()[-n:]
        )
    except OSError:
        return "(no stderr captured)"


def main():
    argv = sys.argv[1:]
    count = "--count" in argv
    stale_ok = "--stale-ok" in argv
    auto = "--auto-refresh" in argv
    args = [a for a in argv if not a.startswith("--")]
    if not args:
        sys.exit(__doc__)
    op, rest = args[0], args[1:]
    if op in ("refs", "def", "sym") and rest:
        index = ensure_fresh(auto_refresh=auto, stale_ok=stale_ok)
        {"refs": cmd_refs, "def": cmd_def, "sym": cmd_sym}[op](rest[0], index, count)
    elif op == "expand" and rest:
        cmd_expand(rest[0], rest[1] if len(rest) > 1 else None)
    elif op == "status":
        cmd_status()
    elif op == "refresh":
        cmd_refresh()
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
