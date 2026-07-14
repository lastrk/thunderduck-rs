#!/usr/bin/env python3
"""Whole-build RSS / concurrency sampler.

Polls /proc every INTERVAL seconds for all live processes whose comm matches a
compiler/build tool (rustc, cc1plus, cc1, clang, gcc, cargo, build-script*).
For each sample it records total RSS across those processes and a per-tool
active count, so we can see:

  * peak concurrent memory of the whole build
  * how many compiler processes run at once over time (parallelism actually
    used vs. headroom for more)

Usage: sampler.py <out.csv> [interval_seconds]
Stops when it receives SIGTERM/SIGINT.

CSV columns: t_s,total_rss_kib,n_rustc,n_cc,n_other_active,busiest_rss_kib,busiest_comm
"""
import os
import signal
import sys
import time

INTERESTING = ("rustc", "cc1plus", "cc1", "clang", "clang++",
               "gcc", "g++", "cc", "c++", "ld", "lld", "mold",
               "build-script", "cargo")

running = True


def stop(*_):
    global running
    running = False


def proc_iter():
    for pid in os.listdir("/proc"):
        if not pid.isdigit():
            continue
        try:
            with open(f"/proc/{pid}/comm") as f:
                comm = f.read().strip()
            if not any(comm.startswith(p) or comm == p for p in INTERESTING):
                continue
            with open(f"/proc/{pid}/statm") as f:
                # fields in pages; field 1 = resident
                rss_pages = int(f.read().split()[1])
            yield comm, rss_pages * (os.sysconf("SC_PAGE_SIZE") // 1024)
        except (FileNotFoundError, ProcessLookupError, PermissionError, IndexError):
            continue


def main():
    out = sys.argv[1]
    interval = float(sys.argv[2]) if len(sys.argv) > 2 else 0.25
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)

    t0 = time.time()
    with open(out, "w") as f:
        f.write("t_s,total_rss_kib,n_rustc,n_cc,n_other_active,busiest_rss_kib,busiest_comm\n")
        while running:
            total = n_rustc = n_cc = n_other = 0
            busiest_rss = 0
            busiest_comm = "-"
            for comm, rss in proc_iter():
                total += rss
                if comm.startswith("rustc"):
                    n_rustc += 1
                elif comm in ("cc1plus", "cc1", "clang", "clang++", "gcc", "g++"):
                    n_cc += 1
                else:
                    n_other += 1
                if rss > busiest_rss:
                    busiest_rss, busiest_comm = rss, comm
            t = time.time() - t0
            f.write(f"{t:.2f},{total},{n_rustc},{n_cc},{n_other},{busiest_rss},{busiest_comm}\n")
            f.flush()
            time.sleep(interval)


if __name__ == "__main__":
    main()
