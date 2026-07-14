#!/usr/bin/env python3
"""RUSTC_WRAPPER that records per-crate wall time and peak RSS.

cargo invokes this as:  rustc_wrap.py <real-rustc> <args...>
We exec the real rustc, then use os.wait4() to read ru_maxrss (the peak
resident set size of that single rustc process, in KiB on Linux).

A CSV row is appended to $PROFILE_LOG for every invocation that compiles a
named crate (the `-vV` / probe invocations cargo makes are passed through but
not logged). Columns:

    kind,crate,crate_type,wall_s,maxrss_kib,emit
"""
import os
import sys
import time

LOG = os.environ.get("PROFILE_LOG")


def find_opt(args, name):
    """Return the value of `--name value` or `--name=value`, else None."""
    pref = name + "="
    for i, a in enumerate(args):
        if a == name and i + 1 < len(args):
            return args[i + 1]
        if a.startswith(pref):
            return a[len(pref):]
    return None


def main():
    real = sys.argv[1]
    args = sys.argv[2:]

    crate = find_opt(args, "--crate-name")
    crate_type = find_opt(args, "--crate-type") or ""
    emit = find_opt(args, "--emit") or ""

    start = time.time()
    pid = os.fork()
    if pid == 0:
        os.execvp(real, [real] + args)
        os._exit(127)
    _, status, rusage = os.wait4(pid, 0)
    wall = time.time() - start

    if LOG and crate:
        # ru_maxrss is KiB on Linux.
        row = f"rustc,{crate},{crate_type},{wall:.3f},{rusage.ru_maxrss},{emit}\n"
        # Append atomically enough for our serialized / low-concurrency use.
        with open(LOG, "a") as f:
            f.write(row)

    code = os.waitstatus_to_exitcode(status)
    sys.exit(code)


if __name__ == "__main__":
    main()
