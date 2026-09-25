#!/usr/bin/env python3
"""How large is cache/index.json at the default caps?

Builds the index schema from section 2.2 with `n` synthetic entries (real-shaped
digests, plausible origin URLs and byte counts) and prints the serialised size.

Usage: python3 index_size.py [entries]
"""
import hashlib
import json
import sys

N = int(sys.argv[1]) if len(sys.argv) > 1 else 500

entries = {}
for i in range(N):
    digest = hashlib.sha256(str(i).encode()).hexdigest()
    entries[digest] = {
        "ext": "jpg",
        "bytes": 3822331 + i,
        "first_seen": "2026-09-24T22:10:03Z",
        "last_used": "2026-09-25T07:41:12Z",
        "source": "space",
        "kind": "wallhaven",
        "origin": f"https://w.wallhaven.cc/full/{digest[:2]}/wallhaven-{digest[:6]}.jpg",
        "origin_key": f"wallhaven:{digest[:6]}",
        "width": 2560,
        "height": 1440,
        "pinned": i % 20 == 0,
    }

doc = {"schema": 1, "seq": 41, "written_at": "2026-09-25T07:41:12Z",
       "root_id": "8f1d0c2e-5c62-4f1f-9c1f-8a2f2b8f6b41",
       "entries": entries, "dangling": []}

raw = json.dumps(doc, separators=(",", ":")).encode()
pretty = json.dumps(doc, indent=2).encode()
print(f"entries={N}  compact={len(raw)} bytes ({len(raw)/1024:.1f} KiB)  "
      f"pretty={len(pretty)} bytes ({len(pretty)/1024:.1f} KiB)")
print(f"per entry: {len(raw)/N:.0f} bytes compact")
