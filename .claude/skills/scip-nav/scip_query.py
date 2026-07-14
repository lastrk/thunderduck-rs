#!/usr/bin/env python3
"""scip-nav: query a rust-analyzer SCIP snapshot for type-accurate code navigation.

Fills the gaps that codegraph (syntactic tree-sitter) and semble (embeddings) cannot:
trait-/type-resolved references, exact definitions, and workspace symbol search — all
from a static `.scip/index.scip` snapshot with ZERO resident language server.

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
    scip_query.py refresh          # (re)generate the SCIP index + freshness fingerprint
    scip_query.py status           # index freshness report (what's out of sync)

Flags: --count (terse integer), --stale-ok (serve stale w/ warning),
       --auto-refresh (regenerate if stale, then query).
Env: SCIP_WORKSPACE (default /workspace).
"""
import sys, os, re, subprocess, time, hashlib

WS = os.environ.get("SCIP_WORKSPACE", "/workspace")
INDEX = os.path.join(WS, ".scip", "index.scip")
FP_PATH = os.path.join(WS, ".scip", "index.fingerprint")

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
        b = buf[i]; i += 1
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
            v, i = _varint(buf, i); yield fn, wt, v
        elif wt == 2:
            ln, i = _varint(buf, i); yield fn, wt, buf[i:i+ln]; i += ln
        elif wt == 1:
            yield fn, wt, buf[i:i+8]; i += 8
        elif wt == 5:
            yield fn, wt, buf[i:i+4]; i += 4
        else:
            raise ValueError(f"unsupported wire type {wt}")

def _packed_varints(buf):
    i, n = 0, len(buf)
    while i < n:
        v, i = _varint(buf, i); yield v

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

def load_documents(path=INDEX):
    if not os.path.exists(path):
        sys.exit(f"error: no SCIP index at {path} — run `scip_query.py refresh` first")
    with open(path, "rb") as f:
        data = f.read()
    docs = []
    for fn, wt, val in _fields(data):
        if fn == 2 and wt == 2:
            docs.append(_parse_document(val))
    return docs

# ---------- freshness (fast, content-accurate, git-based) ----------

def _git(*args):
    return subprocess.check_output(["git", "-C", WS, *args], text=True,
                                   stderr=subprocess.DEVNULL)

def source_fingerprint():
    """Fast fingerprint of the Rust source STATE. Returns hex, or None if not a git repo.

    = HEAD (covers all clean/committed files) + porcelain status (covers adds/dels/mods
    of tracked & untracked) + mtime_ns+size of each dirty path (covers *successive* edits
    to an already-dirty file, which porcelain text alone would miss). A no-op `touch` on a
    clean file is correctly ignored (git reports no content change). ~70 ms here."""
    try:
        head = _git("rev-parse", "HEAD").strip()
        porcelain = _git("status", "--porcelain", "-uall")
    except Exception:
        return None
    h = hashlib.sha256(); h.update(head.encode())
    for line in sorted(porcelain.splitlines()):
        h.update(line.encode())
        path = line[3:]
        if " -> " in path:            # rename: "orig -> new"
            path = path.split(" -> ")[-1]
        try:
            st = os.stat(os.path.join(WS, path.strip()))
            h.update(f"{st.st_mtime_ns}:{st.st_size}".encode())
        except OSError:
            pass
    return h.hexdigest()

def _stored_fp():
    try:
        with open(FP_PATH) as f:
            return f.read().strip()
    except OSError:
        return None

def ensure_fresh(auto_refresh=False, stale_ok=False):
    """Structural guard: called before EVERY query. Fails closed on staleness."""
    cur, stored = source_fingerprint(), _stored_fp()
    if cur is None:
        sys.stderr.write("scip-nav: WARNING — cannot verify freshness (not a git repo); results may be stale.\n")
        return
    if stored is not None and stored == cur:
        return  # provably fresh
    reason = "no fingerprint recorded" if stored is None else "working tree changed since index was built"
    if auto_refresh:
        sys.stderr.write(f"scip-nav: index stale ({reason}) — auto-refreshing...\n")
        cmd_refresh(); return
    if stale_ok:
        sys.stderr.write(f"scip-nav: WARNING — serving STALE index ({reason}); --stale-ok set.\n")
        return
    sys.exit(f"scip-nav: ERROR — index is STALE ({reason}); refusing to return possibly-wrong results.\n"
             f"  fix:  python3 {os.path.relpath(__file__, WS)} refresh\n"
             f"  or:   re-run with --auto-refresh (regenerate now) or --stale-ok (query anyway)")

# ---------- matching ----------

def _matches(symbol, name):
    if not symbol:
        return False
    return re.search(r"(?:^|[^A-Za-z0-9_])" + re.escape(name) + r"(?:[^A-Za-z0-9_]|$)", symbol) is not None

def _line(rng):
    return (rng[0] + 1) if rng else 0

# ---------- commands ----------

def cmd_refs(name, count_only=False):
    hits = {}
    for path, occs in load_documents():
        for symbol, roles, rng in occs:
            if _matches(symbol, name) and not (roles & DEFINITION):
                hits.setdefault(path, []).append(_line(rng))
    total = sum(len(v) for v in hits.values())
    if count_only:
        print(total); return
    print(f"references to '{name}': {total} across {len(hits)} file(s)\n")
    for path in sorted(hits, key=lambda p: -len(hits[p])):
        lines = sorted(hits[path])
        preview = ", ".join(map(str, lines[:12]))
        more = "" if len(lines) <= 12 else f", +{len(lines)-12} more"
        print(f"  {path}  ({len(lines)})\n    lines: {preview}{more}")

def cmd_def(name, count_only=False):
    defs = []
    for path, occs in load_documents():
        for symbol, roles, rng in occs:
            if _matches(symbol, name) and (roles & DEFINITION):
                defs.append((path, _line(rng), symbol))
    if count_only:
        print(len(defs)); return
    print(f"definition(s) of '{name}': {len(defs)}\n")
    for path, line, symbol in sorted(defs):
        print(f"  {path}:{line}\n    {symbol}")

def cmd_sym(name, count_only=False):
    syms = {}
    for path, occs in load_documents():
        for symbol, roles, rng in occs:
            if (roles & DEFINITION) and symbol and name.lower() in symbol.lower():
                syms.setdefault(symbol, (path, _line(rng)))
    if count_only:
        print(len(syms)); return
    print(f"symbols matching '{name}': {len(syms)}\n")
    for symbol, (path, line) in sorted(syms.items(), key=lambda kv: kv[1]):
        desc = symbol.split(" ")[-1] if " " in symbol else symbol
        print(f"  {path}:{line}  {desc}")

def cmd_status():
    if not os.path.exists(INDEX):
        print("index: MISSING — run `scip_query.py refresh`"); return
    cur, stored = source_fingerprint(), _stored_fp()
    size = os.path.getsize(INDEX) / (1024*1024)
    built = time.ctime(os.path.getmtime(INDEX))
    print(f"index: {INDEX}\n  size: {size:.1f} MB   built: {built}")
    if cur is None:
        print("  freshness: UNVERIFIABLE (not a git repo)"); return
    if stored is None:
        print("  freshness: UNKNOWN (no fingerprint; run refresh)"); return
    if stored == cur:
        print("  freshness: FRESH ✓ (matches working tree)"); return
    print("  freshness: STALE ✗ (working tree changed since build) — run refresh")
    try:
        dirty = [l for l in _git("status", "--porcelain", "-uall").splitlines() if l.strip().endswith(".rs")]
        if dirty:
            print("  changed .rs (sample):")
            for l in dirty[:8]:
                print(f"    {l}")
    except Exception:
        pass

def cmd_refresh():
    os.makedirs(os.path.join(WS, ".scip"), exist_ok=True)
    print("generating SCIP index (rust-analyzer scip — ~15s, ~3GB transient)...")
    cmd = f"ulimit -v 7500000 2>/dev/null; nice -n 15 timeout 400 rust-analyzer scip {WS} --output {INDEX}"
    rc = subprocess.call(["bash", "-c", cmd])
    if rc == 0 and os.path.exists(INDEX):
        fp = source_fingerprint()
        if fp:
            with open(FP_PATH, "w") as f:
                f.write(fp)
        print(f"ok: {INDEX} ({os.path.getsize(INDEX)/(1024*1024):.1f} MB); fingerprint {'recorded' if fp else 'SKIPPED (not a git repo)'}")
    else:
        sys.exit(f"error: rust-analyzer scip failed (rc={rc})")

def _show_region(path, pattern, before=2, after=24, max_hits=6):
    rx = re.compile(pattern)
    lines = open(path, encoding="utf-8", errors="replace").read().splitlines()
    hits = [i for i, l in enumerate(lines) if rx.search(l)]
    if not hits:
        print(f"no match for /{pattern}/ in {path} (try a broader pattern)"); return
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
    crate-scoped, needs a green tree. Fills the last gap SCIP/codegraph/semble cannot:
    the actual code generated by macro_rules!/derive/proc-macros. Cached + fingerprinted;
    regenerates only when the source state changed."""
    pkg = CRATE_ALIASES.get(crate, crate)
    os.makedirs(os.path.join(WS, ".scip"), exist_ok=True)
    out = os.path.join(WS, ".scip", f"expanded-{pkg}.rs")
    fpf, errf = out + ".fp", out + ".err"
    cur = source_fingerprint()
    fresh = os.path.exists(out) and cur is not None and _read(fpf) == cur
    if not fresh:
        sys.stderr.write(f"scip-nav: expanding {pkg} via nightly rustc "
                         f"(compile-bound ~cargo-check cost; needs a green tree)...\n")
        cmd = (f"nice -n 15 timeout 600 cargo +nightly rustc -p {pkg} --lib "
               f"-- -Zunpretty=expanded 2>{errf}")
        with open(out, "w") as f:
            rc = subprocess.call(["bash", "-c", cmd], stdout=f, cwd=WS)
        if rc != 0 or os.path.getsize(out) == 0:
            sys.exit(f"scip-nav: expand FAILED (rc={rc}) — tree not green, nightly missing, or "
                     f"cargo lock held by a concurrent build. stderr tail:\n{_tail(errf, 20)}")
        if cur:
            with open(fpf, "w") as f:
                f.write(cur)
        sys.stderr.write(f"scip-nav: cached {out} ({os.path.getsize(out)//1024} KB)\n")
    if not pattern:
        n = sum(1 for _ in open(out, encoding="utf-8", errors="replace"))
        print(f"expanded {pkg}: {out} ({n} lines). Slice it, e.g. "
              f"`expand {crate} 'fn children'` or `expand {crate} 'bail_boundary'`.")
        return
    _show_region(out, pattern)

def _read(p):
    try:
        return open(p).read().strip()
    except OSError:
        return None

def _tail(p, n):
    try:
        return "\n".join(open(p, encoding="utf-8", errors="replace").read().splitlines()[-n:])
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
        ensure_fresh(auto_refresh=auto, stale_ok=stale_ok)     # <-- structural freshness guard
        {"refs": cmd_refs, "def": cmd_def, "sym": cmd_sym}[op](rest[0], count)
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
