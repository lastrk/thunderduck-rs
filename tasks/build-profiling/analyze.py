#!/usr/bin/env python3
"""Summarize a profiling run captured by run-profile.sh.

Usage: analyze.py results/<label>
"""
import csv
import sys
from pathlib import Path


def mib(kib):
    return kib / 1024.0


def load_rustc(path):
    rows = []
    if not path.exists():
        return rows
    with open(path) as f:
        for r in csv.DictReader(f):
            try:
                rows.append({
                    "crate": r["crate"],
                    "type": r["crate_type"],
                    "wall": float(r["wall_s"]),
                    "rss": int(r["maxrss_kib"]),
                })
            except (ValueError, KeyError):
                continue
    return rows


def load_sampler(path):
    rows = []
    if not path.exists():
        return rows
    with open(path) as f:
        for r in csv.DictReader(f):
            rows.append({
                "t": float(r["t_s"]),
                "total": int(r["total_rss_kib"]),
                "rustc": int(r["n_rustc"]),
                "cc": int(r["n_cc"]),
                "busiest_rss": int(r["busiest_rss_kib"]),
                "busiest": r["busiest_comm"],
            })
    return rows


def main():
    out = Path(sys.argv[1])
    rustc = load_rustc(out / "rustc.csv")
    samp = load_sampler(out / "sampler.csv")

    print(f"=== {out.name} ===\n")

    if rustc:
        total_wall = sum(r["wall"] for r in rustc)
        print(f"rustc invocations (compiled crates): {len(rustc)}")
        print(f"sum of rustc wall time (serial):     {total_wall:7.1f}s")
        peak = max(rustc, key=lambda r: r["rss"])
        print(f"peak single-rustc RSS:               {mib(peak['rss']):7.0f} MiB  ({peak['crate']})\n")

        print("Top 15 crates by peak RSS:")
        print(f"  {'crate':32} {'type':14} {'RSS MiB':>9} {'wall s':>8}")
        for r in sorted(rustc, key=lambda r: -r["rss"])[:15]:
            print(f"  {r['crate'][:32]:32} {r['type'][:14]:14} {mib(r['rss']):9.0f} {r['wall']:8.1f}")

        print("\nTop 15 crates by wall time:")
        print(f"  {'crate':32} {'type':14} {'RSS MiB':>9} {'wall s':>8}")
        for r in sorted(rustc, key=lambda r: -r["wall"])[:15]:
            print(f"  {r['crate'][:32]:32} {r['type'][:14]:14} {mib(r['rss']):9.0f} {r['wall']:8.1f}")

    if samp:
        peak = max(samp, key=lambda r: r["total"])
        print("\n--- whole-build timeline (sampler) ---")
        print(f"samples:                 {len(samp)}  over {samp[-1]['t']:.0f}s")
        print(f"peak total RSS:          {mib(peak['total']):7.0f} MiB  at t={peak['t']:.0f}s "
              f"(busiest: {peak['busiest']} {mib(peak['busiest_rss']):.0f} MiB)")
        max_rustc = max(r["rustc"] for r in samp)
        max_cc = max(r["cc"] for r in samp)
        print(f"max concurrent rustc:    {max_rustc}")
        print(f"max concurrent cc1plus:  {max_cc}")

        # Time spent with exactly 1 active compiler vs idle vs >1.
        dt = samp[1]["t"] - samp[0]["t"] if len(samp) > 1 else 0.2
        idle = one = many = 0.0
        cc_time = rustc_time = 0.0
        for r in samp:
            active = r["rustc"] + r["cc"]
            if active == 0:
                idle += dt
            elif active == 1:
                one += dt
            else:
                many += dt
            if r["cc"] > 0:
                cc_time += dt
            if r["rustc"] > 0:
                rustc_time += dt
        print(f"\napprox wall in each state (dt={dt:.2f}s):")
        print(f"  idle (no compiler):    {idle:7.1f}s")
        print(f"  exactly 1 compiler:    {one:7.1f}s   <- serialized; candidate for -j>1")
        print(f"  >1 compiler:           {many:7.1f}s")
        print(f"  any cc1plus active:    {cc_time:7.1f}s   (C/C++; e.g. duckdb-sys amalgamation)")
        print(f"  any rustc active:      {rustc_time:7.1f}s")


if __name__ == "__main__":
    main()
