#!/usr/bin/env python3
"""Count how many store slots a write actually moved: LastSet versus LastUse.

  python3 churn2.py a.plist b.plist

Reproduces the count half of macos.md [V5b]. Reads two snapshots of Index.plist (the
w0.plist / w1.plist pair the allspaces recipe writes) and reports, per timestamp field,
how many slots changed and which ones. `LastSet` is the write marker; `LastUse` is bumped
on every in-use slot by any write (and when the frontmost Space changes), which is why a
one-image change shows up as roughly twenty changed lines.

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
    for field in ("LastSet", "LastUse"):
        bumped = [k for k in a if k in b and a[k].get(field) != b[k].get(field)]
        print(f"{field}: {len(bumped)} of {len(a)}")
        for label in sorted(bumped):
            print(f"   * {label}: {a[label].get(field)} -> {b[label].get(field)}")


if __name__ == "__main__":
    main(sys.argv)
