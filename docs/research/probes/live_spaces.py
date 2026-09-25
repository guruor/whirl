#!/usr/bin/env python3
"""Which Space nodes are in use? LastUse age buckets for every Space node in the store.

  python3 live_spaces.py [plist]

Reproduces macos.md [V6]. Defaults to the live store; pass a snapshot path to read one.
The buckets are relative to now, so the counts creep with the clock: the number of in-use
Space nodes is stable, while the aging bins drift by a day between runs. That is the point
of the measurement, not noise in it.

Read-only. Imports wallpaper_store.py from this directory for the nested-plist decoding.
"""

import datetime
import plistlib
import sys

from wallpaper_store import store_path


BUCKETS = ("<1h", "<24h", "<7d", "<30d", "<1y", "older", "unset")


def main(argv):
    path = argv[1] if len(argv) > 1 else store_path()
    with open(path, "rb") as handle:
        top = plistlib.load(handle)
    now = datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None)
    buckets = dict.fromkeys(BUCKETS, 0)
    total = 0
    for _space, node in top.get("Spaces", {}).items():
        total += 1
        slot = (node.get("Default") or {}).get("Desktop") if isinstance(node, dict) else None
        when = slot.get("LastUse") if isinstance(slot, dict) else None
        if not isinstance(when, datetime.datetime):
            buckets["unset"] += 1
            continue
        age = (now - when).total_seconds()
        key = ("<1h" if age < 3600 else "<24h" if age < 86400 else "<7d" if age < 7 * 86400
               else "<30d" if age < 30 * 86400 else "<1y" if age < 365 * 86400 else "older")
        buckets[key] += 1
    print(f"# {path}")
    print(f"now (UTC): {now}")
    for key in BUCKETS:
        print(f"  LastUse {key:6s}: {buckets[key]}")
    print(f"  total Space nodes: {total}")


if __name__ == "__main__":
    main(sys.argv)
