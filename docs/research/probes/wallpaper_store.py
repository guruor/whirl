#!/usr/bin/env python3
"""Read and diff the macOS wallpaper store.

Read-only. Nothing here writes to Index.plist; use wp_set.m for writes.

  python3 wallpaper_store.py layers                 # one slot per layer, per-Space variation
  python3 wallpaper_store.py dump [space-uuid]      # decoded tree of a Space node + a display node
  python3 wallpaper_store.py current <space-uuid>   # one Space node's Desktop slot, as key=value
                                                    # lines (space, provider, type, url, path,
                                                    # lastset, lastuse)
  python3 wallpaper_store.py current --holds <path> # the Space node holding this image, the same
                                                    # lines; what scripts/desktop-*.sh use to find
                                                    # the node the image on screen belongs to
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
import urllib.parse

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


def space_desktop(node):
    """The `Default.Desktop` slot of one Space node, or {} when it has none."""
    if not isinstance(node, dict):
        return {}
    slot = (node.get("Default") or {}).get("Desktop")
    return slot if isinstance(slot, dict) else {}


def node_path(node):
    """The filesystem path of a Space node's `Default.Desktop` image, or None."""
    provider, cfg = image_choice(space_desktop(node))
    url = cfg.get("url")
    if isinstance(url, dict):
        url = url.get("relative")
    return file_url_path(url)


def image_choice(slot):
    """(provider, decoded Configuration) of a slot's first choice."""
    choices = ((slot.get("Content") or {}).get("Choices")) or []
    if not choices:
        return None, {}
    cfg = dec(choices[0].get("Configuration"))
    return choices[0].get("Provider"), cfg if isinstance(cfg, dict) else {}


def file_url_path(url):
    """The filesystem path behind a `file://` URL, or None when there is not one."""
    if not isinstance(url, str) or not url.startswith("file://"):
        return None
    rest = url[len("file://"):]
    if not rest.startswith("/"):
        return None
    return urllib.parse.unquote(rest)


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


def cmd_current(path, selector=None):
    """One Space node's `Desktop` slot, one `key=value` line per field.

    The store does not record which Space is frontmost, and the proxy the README used to name
    (the node whose `Default.Desktop.LastUse` is newest) is not it: measured 2026-09-26, the
    newest-`LastUse` node held `wallhaven-28y8x9.jpg` while `desktopImageURLForScreen`, which
    reflects the live Space's node, returned `wh-5dzd11.jpg` from a different Space node. Every
    in-use node is bumped by the same write within about a millisecond, so the newest of them is
    whichever the wallpaper agent happened to touch last.

    So the caller says which node it means, and there are two ways to say it:

      a Space uuid      the node with that key, no guessing
      --holds <path>    the node whose Desktop image is this file, which is how a live read
                        (`wp_probe`, `desktopImageURLForScreen`) is turned into a store node:
                        read what is on screen, then ask the store which node holds it. Accepts
                        a plain path or the `file://` url the store itself prints. 13 Space nodes
                        held the image that was on screen on 2026-09-26 07:11 UTC (an old rotator
                        painted the same file into Spaces that no longer exist), so when several
                        nodes hold it this takes the one whose LastUse is newest and says so on
                        stderr, and it refuses when the two newest were used within the same
                        second: that is one write's LastUse batch, not an answer.

    Exits non-zero when the node holds no image URL (an aerial or a solid colour), when nothing
    holds the path, and when holding it does not name one node.
    """
    with open(path, "rb") as handle:
        top = plistlib.load(handle)
    spaces = {
        uuid: node for uuid, node in (top.get("Spaces") or {}).items() if isinstance(node, dict)
    }
    if not spaces:
        sys.exit("no Space nodes in the store")

    if selector is None:
        sys.exit(
            "the store does not say which Space is frontmost; pass a Space uuid, or "
            "--holds <path> for the node holding an image you already know is on screen"
        )
    if selector.startswith("--holds=") or selector == "--holds":
        wanted = selector.split("=", 1)[1] if "=" in selector else None
        if wanted is None:
            sys.exit("--holds needs a path or a file:// url")
        wanted_path = file_url_path(wanted) if wanted.startswith("file://") else wanted
        live = [uuid for uuid in spaces if node_path(spaces[uuid]) == wanted_path]
        if not live:
            sys.exit(f"no Space node holds {wanted_path!r}")
        if len(live) > 1:
            # Spaces that no longer exist keep whatever was last painted into them, so the same
            # file can be held by more than one node. LastUse separates the dead from the live
            # one here (2026-09-26 07:11 UTC: Spaces[''] at 07:05:44, the next holder at
            # 06:33:28 and then months back), but only when the write batches differ.
            def last_use(uuid):
                value = space_desktop(spaces[uuid]).get("LastUse")
                return value if isinstance(value, datetime.datetime) else datetime.datetime.min

            ranked = sorted(live, key=last_use, reverse=True)
            if (last_use(ranked[0]) - last_use(ranked[1])).total_seconds() < 1:
                sys.exit(
                    f"{len(ranked)} Space nodes hold {wanted_path!r} and the two most recently "
                    f"used are within the same second ({', '.join(ranked[:4])}); which one is on "
                    "screen is not in the store"
                )
            print(
                f"# {len(ranked)} Space nodes hold this image; taking the most recently used, "
                f"Spaces[{ranked[0]}] LastUse {last_use(ranked[0])} (next: Spaces[{ranked[1]}] "
                f"{last_use(ranked[1])})",
                file=sys.stderr,
            )
            live = ranked
        live = live[0]
    else:
        if selector not in spaces:
            sys.exit(f"no Space node {selector!r} in the store")
        live = selector

    slot = space_desktop(spaces[live])
    provider, cfg = image_choice(slot)
    url = cfg.get("url")
    if isinstance(url, dict):
        url = url.get("relative")
    as_path = file_url_path(url)

    def line(key, value):
        return f"{key}={'' if value is None else value}"

    print(line("space", live))
    print(line("provider", provider))
    print(line("type", cfg.get("type")))
    print(line("url", url))
    print(line("path", as_path))
    print(line("lastset", slot.get("LastSet")))
    print(line("lastuse", slot.get("LastUse")))
    if not isinstance(url, str) or not url:
        sys.exit(
            f"Spaces[{live}].Default.Desktop holds no image url "
            f"(provider={provider!r}, assetID={cfg.get('assetID')!r})"
        )
    if as_path is None:
        sys.exit(f"Spaces[{live}].Default.Desktop names {url!r}, which is not a file:// url")


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
    elif cmd == "current":
        rest = argv[2:]
        if not rest:
            cmd_current(path)
        elif rest[0] == "--holds":
            if len(rest) < 2:
                sys.exit("--holds needs a path")
            cmd_current(path, "--holds=" + rest[1])
        else:
            cmd_current(path, rest[0])
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
