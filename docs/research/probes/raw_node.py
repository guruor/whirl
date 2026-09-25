#!/usr/bin/env python3
"""Print the raw Space-node timestamps from one or more store snapshots, without decoding blobs.

  python3 raw_node.py l0.plist [l1.plist ...] [--space UUID]

Reproduces the raw read used in macos.md [V7]: which Space node a launchd-run write landed
on and its raw `LastSet` / `LastUse`, read straight out of the plist. Pass more than one
snapshot to compare them, e.g. the pair a wrapper run writes before and after.

The node is selected the way the rest of the probes select it: the Space whose
`Default.Desktop.LastUse` is newest is the one on screen. Use `--space` to name one instead.

Read-only.
"""

import datetime
import plistlib
import sys

from wallpaper_store import store_path  # noqa: F401  (kept for parity with the other probes)


def newest_space(top):
    spaces = top.get("Spaces", {})
    return max(
        (s for s in spaces if isinstance(spaces[s], dict)),
        key=lambda s: (spaces[s].get("Default", {}).get("Desktop", {}) or {})
        .get("LastUse", datetime.datetime.min),
    )


def main(argv):
    args = argv[1:]
    space = None
    if "--space" in args:
        i = args.index("--space")
        space = args[i + 1]
        del args[i:i + 2]
    if not args:
        sys.exit(__doc__)
    for path in args:
        with open(path, "rb") as handle:
            top = plistlib.load(handle)
        uuid = space or newest_space(top)
        node = top.get("Spaces", {}).get(uuid)
        print(f"# {path}   Spaces[{uuid}]")
        if not isinstance(node, dict):
            print("   <no such Space node>")
            continue
        for layer, holder in (("Default", node),):
            slot = (holder.get(layer) or {}).get("Desktop")
            if isinstance(slot, dict):
                print(f"   {layer}.Desktop   LastSet={slot.get('LastSet')} LastUse={slot.get('LastUse')}")
        for duuid, dnode in (node.get("Displays") or {}).items():
            slot = (dnode or {}).get("Desktop")
            if isinstance(slot, dict):
                print(f"   Displays[{duuid}].Desktop  LastSet={slot.get('LastSet')} "
                      f"LastUse={slot.get('LastUse')}")


if __name__ == "__main__":
    main(sys.argv)
