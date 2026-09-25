#!/usr/bin/env python3
"""Size distribution of real Wallhaven candidates at the spec's admission floor.

Reads the saved search responses (see the doc's evidence table) and prints
file_size / dimension stats, then the implied byte total of a count-only cap.

Usage: python3 size_stats.py wallhaven-p1.json [wallhaven-p2.json ...]
"""
import json
import sys

sizes = []
for path in sys.argv[1:]:
    data = json.load(open(path))
    for w in data.get("data", []):
        sizes.append((w.get("file_size"), w.get("dimension_x"), w.get("dimension_y"),
                      w.get("file_type")))

sizes = [s for s in sizes if s[0]]
sizes.sort()
mb = [s[0] / 1_000_000 for s in sizes]


def pct(p):
    return mb[min(len(mb) - 1, int(len(mb) * p / 100))]


print(f"n={len(mb)}  min={mb[0]:.2f} MB  p50={pct(50):.2f} MB  p90={pct(90):.2f} MB  "
      f"max={mb[-1]:.2f} MB  total={sum(mb):.1f} MB")
print(f"mean={sum(mb)/len(mb):.2f} MB  per-file spread = {mb[-1]/mb[0]:.0f}x")
for keep in (40, 100, 500):
    print(f"count cap keep={keep}: worst case {keep} x {mb[-1]:.2f} MB = "
          f"{keep*mb[-1]:8.1f} MB, best case {keep} x {mb[0]:.2f} MB = {keep*mb[0]:7.1f} MB, "
          f"at the sample mean {keep*sum(mb)/len(mb):7.1f} MB")
