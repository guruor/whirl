#!/usr/bin/env python3
"""Group the LastUse churn by timestamp: one write bumps every in-use slot in the same instant.

  python3 lastuse_groups.py a.plist b.plist

Reproduces the grouping half of macos.md [V5b]. Reads the same two snapshots as churn2.py
and prints how many slots had `LastUse` bumped, how many distinct new timestamps those slots
carry, and the spread between the earliest and latest of them. If the spread is well under a
millisecond, the bump is one event, not twenty independent ones.

Read-only. Imports wallpaper_store.py from this directory.
"""

import plistlib
import sys

from wallpaper_store import collect


def load(path):
    with open(path, "rb") as handle:
        return collect(plistlib.load(handle))


def main(argv):
    if len(argv) < 3:
        sys.exit(__doc__)
    a, b = load(argv[1]), load(argv[2])
    bumped = [k for k in a if k in b and a[k].get("LastUse") != b[k].get("LastUse")]
    stamps = [b[k].get("LastUse") for k in bumped]
    print(f"LastUse changed on {len(bumped)} of {len(a)} slots")
    distinct = sorted({s for s in stamps if s is not None})
    print(f"distinct new timestamps: {len(distinct)}")
    if len(distinct) > 1:
        spread = (distinct[-1] - distinct[0]).total_seconds()
        print(f"spread: {spread * 1000:.4f} ms")
    print("by timestamp:")
    for stamp in distinct:
        labels = sorted(k for k in bumped if b[k].get("LastUse") == stamp)
        print(f"   {stamp}  ({len(labels)})")
        for label in labels:
            print(f"      {label}")


if __name__ == "__main__":
    main(sys.argv)
