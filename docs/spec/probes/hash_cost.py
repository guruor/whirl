#!/usr/bin/env python3
"""What does hashing a downloaded image actually cost on this machine?

Writes a 20 MB blob to the scratch dir, then times (a) a plain read and
(b) a read plus SHA-256, three runs each, and prints the millisecond cost.

Usage: python3 hash_cost.py [megabytes]
"""
import hashlib
import os
import sys
import time

MB = int(sys.argv[1]) if len(sys.argv) > 1 else 20
path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "blob.bin")
block = os.urandom(1 << 20)
with open(path, "wb") as f:
    for _ in range(MB):
        f.write(block)
size = os.path.getsize(path)


def read_only():
    with open(path, "rb") as f:
        while f.read(1 << 20):
            pass


def read_hash():
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while True:
            chunk = f.read(1 << 20)
            if not chunk:
                break
            h.update(chunk)
    return h.hexdigest()


print(f"blob {size/1_000_000:.1f} MB at {path}")
for fn in (read_only, read_hash):
    times = []
    for _ in range(3):
        t = time.perf_counter()
        fn()
        times.append((time.perf_counter() - t) * 1000)
    print(f"{fn.__name__:10s} ms: " + "  ".join(f"{t:.1f}" for t in times))
os.unlink(path)
print("blob removed")
