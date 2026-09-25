#!/usr/bin/env python3
"""Read and diff the macOS wallpaper store.

Read-only. Nothing here writes to Index.plist; use wp_set.m for writes.

  python3 wallpaper_store.py layers                 # one slot per layer, per-Space variation
  python3 wallpaper_store.py dump [space-uuid]      # decoded tree of a Space node + a display node
  python3 wallpaper_store.py diff a.plist b.plist   # every slot whose choice or LastSet changed,
                                                    # plus LastUse churn

The store is a plist of plists: each slot's real content sits inside one or two nested binary
plists, which is why plutil -p is not enough:

  node[<Space or Display>][slot] = {
      'Content': {'Choices': [{'Provider': ..., 'Configuration': <bplist blob>}],
                  'Shuffle': '$null', 'EncodedOptionValues': <bplist blob>},
      'LastSet': <UTC datetime>, 'LastUse': <UTC datetime>}

  Configuration (still image)  -> {'type': 'imageFile'|'systemDesktopPicture',
                                   'url': {'relative': 'file://...'}}
  Configuration (aerial)       -> {'assetID': '8BE8B524-...'}
  EncodedOptionValues          -> {'values': {'placement': {'_0': {'id': 'Crop'}}, 'color': {...}}}

Timestamps are UTC; LocalTime is whatever the shell reports.
"""

import datetime
import plistlib
import pprint
import sys

STORE = ("~/Library/Application Support/com.apple.wallpaper/Store/Index.plist")


def store_path():
    import os

    return os.path.expanduser(STORE)


def dec(value):
    """Decode one nested binary plist, or return the value unchanged."""
    if isinstance(value, (bytes, bytearray)) and bytes(value[:8]) == b"bplist00":
        try:
            return plistlib.loads(bytes(value))
        except Exception:  # noqa: BLE001
            return {"<undecodable>": bytes(value)[:60].hex()}
    return value


def slot_summary(slot, include_lastuse=True):
    """One line per slot: provider, type, url or assetID, placement, timestamps.

    include_lastuse=False drops LastUse, because LastUse is bumped on every in-use slot
    whenever any wallpaper is written (and when the frontmost Space changes), so it turns
    a one-image change into a twenty-line diff.
    """
    if not isinstance(slot, dict):
        return "<absent>"
    content = slot.get("Content", {})
    choices = content.get("Choices", []) or []
    parts = []
    for choice in choices:
        cfg = dec(choice.get("Configuration")) or {}
        url = cfg.get("url")
        rel = url.get("relative") if isinstance(url, dict) else url
        parts.append(
            f"provider={choice.get('Provider')!r} type={cfg.get('type')!r} "
            f"url={rel!r} assetID={cfg.get('assetID')!r}"
        )
    opts = dec(content.get("EncodedOptionValues"))
    placement = "-"
    if isinstance(opts, dict):
        placement = (
            ((opts.get("values") or {}).get("placement") or {}).get("picker") or {}
        ).get("_0", {}).get("id", "-")
    shuffle = content.get("Shuffle")
    tail = f"LastSet={slot.get('LastSet')}"
    if include_lastuse:
        tail += f" LastUse={slot.get('LastUse')}"
    return (
        f"{' '.join(parts) or '<no choices>'} placement={placement} shuffle={shuffle!r} {tail}"
    )


def walk(top, fn):
    """Call fn(label, slot) for every slot in the store."""
    for name in ("AllSpacesAndDisplays", "SystemDefault"):
        node = top.get(name)
        if isinstance(node, dict):
            for slot in ("Desktop", "Idle"):
                if slot in node:
                    fn(f"{name}.{slot}", node[slot])
    for uuid, node in top.get("Displays", {}).items():
        if isinstance(node, dict):
            for slot in ("Desktop", "Idle"):
                if slot in node:
                    fn(f"Displays[{uuid}].{slot}", node[slot])
    for space, node in top.get("Spaces", {}).items():
        if not isinstance(node, dict):
            continue
        default = node.get("Default", {})
        for slot in ("Desktop", "Idle"):
            if slot in default:
                fn(f"Spaces[{space}].Default.{slot}", default[slot])
        for uuid, node2 in (node.get("Displays") or {}).items():
            for slot in ("Desktop", "Idle"):
                if slot in node2:
                    fn(f"Spaces[{space}].Displays[{uuid}].{slot}", node2[slot])


def collect(top):
    out = {}
    walk(top, lambda label, slot: out.__setitem__(label, slot))
    return out


def cmd_layers(path):
    with open(path, "rb") as handle:
        top = plistlib.load(handle)
    print(f"### top-level nodes: {sorted(top.keys())}")
    for name in sorted(top):
        node = top[name]
        if isinstance(node, dict):
            print(f"  {name}: {len(node)} children")
    print()
    print("### the same slot from each layer")
    for name in ("AllSpacesAndDisplays", "SystemDefault"):
        for slot in ("Desktop", "Idle"):
            node = top.get(name, {})
            if isinstance(node, dict) and slot in node:
                print(f"  {name}.{slot}\n      {slot_summary(node[slot])}")
    for uuid, node in sorted(top.get("Displays", {}).items()):
        if isinstance(node, dict) and "Desktop" in node:
            print(f"  Displays[{uuid}].Desktop\n      {slot_summary(node['Desktop'])}")
    print()
    print("### per-Space variation")
    urls = []
    for space, node in top.get("Spaces", {}).items():
        slot = (node.get("Default") or {}).get("Desktop") if isinstance(node, dict) else None
        if slot is None:
            continue
        cfg = dec((slot.get("Content", {}).get("Choices") or [{}])[0].get("Configuration")) or {}
        url = cfg.get("url")
        rel = url.get("relative") if isinstance(url, dict) else url
        if rel:
            urls.append(rel)
    print(f"  Space nodes: {len(top.get('Spaces', {}))}, with an image: {len(urls)}, "
          f"distinct images: {len(set(urls))}")
    print()
    print("### which Space nodes are in use (LastUse age, UTC vs local now)")
    now = datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None)
    buckets = {"<1h": 0, "<24h": 0, "<7d": 0, "<30d": 0, "<1y": 0, "older": 0, "unset": 0}
    for space, node in top.get("Spaces", {}).items():
        slot = (node.get("Default") or {}).get("Desktop") if isinstance(node, dict) else None
        when = slot.get("LastUse") if isinstance(slot, dict) else None
        if not isinstance(when, datetime.datetime):
            buckets["unset"] += 1
            continue
        age = (now - when).total_seconds()
        key = ("<1h" if age < 3600 else "<24h" if age < 86400 else "<7d" if age < 7 * 86400
               else "<30d" if age < 30 * 86400 else "<1y" if age < 365 * 86400 else "older")
        buckets[key] += 1
    for key, count in buckets.items():
        print(f"  LastUse {key:6s}: {count}")


def cmd_dump(path, space=None):
    with open(path, "rb") as handle:
        top = plistlib.load(handle)
    spaces = top.get("Spaces", {})
    if space is None:
        newest = max(
            (s for s in spaces if isinstance(spaces[s], dict)),
            key=lambda s: (spaces[s].get("Default", {}).get("Desktop", {}) or {}).get("LastUse")
            or datetime.datetime.min,
        )
        space = newest
        print(f"# no space given, using the most recently used one: {space}")
    node = spaces.get(space)
    if node is None:
        sys.exit(f"no Space node {space!r}")
    print(f"### Spaces[{space}] decoded")
    pretty = {}
    for key, value in node.items():
        if key == "Displays":
            pretty[key] = {
                uuid: {k2: (dec(v2) if k2 == "Content" else v2) for k2, v2 in dnode.items()}
                for uuid, dnode in value.items()
            }
        else:
            pretty[key] = value
    pprint.pprint(pretty, width=150, sort_dicts=False)
    default = node.get("Default", {})
    for slot in ("Desktop", "Idle"):
        if slot in default:
            print(f"\n### Spaces[{space}].Default.{slot}, blobs decoded")
            pprint.pprint(
                {k: (dec(v) if k == "Content" else v) for k, v in default[slot].items()},
                width=150, sort_dicts=False,
            )


def cmd_diff(path_a, path_b):
    with open(path_a, "rb") as handle:
        a = collect(plistlib.load(handle))
    with open(path_b, "rb") as handle:
        b = collect(plistlib.load(handle))
    print(f"slots: A={len(a)} B={len(b)}")
    added, removed = sorted(set(b) - set(a)), sorted(set(a) - set(b))
    print(f"added={len(added)} removed={len(removed)}")
    for label in added:
        print(f"   + {label} -> {slot_summary(b[label])}")
    for label in removed:
        print(f"   - {label}")
    changed = 0
    for label in sorted(set(a) & set(b)):
        sa = slot_summary(a[label], include_lastuse=False)
        sb = slot_summary(b[label], include_lastuse=False)
        if sa != sb:
            changed += 1
            print(f"   ~ {label}\n       A {sa}\n       B {sb}")
    print(f"changed (choice or LastSet): {changed}")
    for field in ("LastSet", "LastUse"):
        bumped = [k for k in a if k in b and a[k].get(field) != b[k].get(field)]
        print(f"{field}: changed on {len(bumped)} of {len(a)} slots")
        for label in bumped[:25]:
            print(f"   * {label}: {a[label].get(field)} -> {b[label].get(field)}")
    print()
    print("note: LastUse is bumped on every in-use slot by any write, and also when the")
    print("frontmost Space changes, so it is not a write marker. LastSet is.")


def main(argv):
    if len(argv) < 2:
        sys.exit(__doc__)
    cmd = argv[1]
    path = store_path()
    if cmd == "layers":
        cmd_layers(argv[2] if len(argv) > 2 else path)
    elif cmd == "dump":
        cmd_dump(path, argv[2] if len(argv) > 2 else None)
    elif cmd == "diff":
        if len(argv) < 4:
            sys.exit("diff needs two snapshot paths")
        cmd_diff(argv[2], argv[3])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
