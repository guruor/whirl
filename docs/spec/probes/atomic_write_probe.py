#!/usr/bin/env python3
"""Does an in-place state rewrite get observed torn, and does temp+rename avoid it?

Two modes, same payload, same number of writer iterations, one reader process
per mode in a tight loop:

  inplace  writer opens the target with "w" (truncate) and writes 512 KiB of JSON
  replace  writer writes the same bytes to a sibling temp file, fsyncs, then
           os.replace()s it onto the target

The reader reads the whole file and json.loads() it. Any read that raises or
parses to something without the expected "seq" key is counted as a bad read.

Usage: python3 atomic_write_probe.py [iterations]
"""
import json
import multiprocessing as mp
import os
import shutil
import sys
import tempfile
import time

PAYLOAD_LEN = 512 * 1024
ITERATIONS = int(sys.argv[1]) if len(sys.argv) > 1 else 400


def payload(seq):
    filler = "x" * (PAYLOAD_LEN - len(str(seq)))
    return json.dumps({"seq": seq, "history": [filler]}, separators=(",", ":")).encode()


def writer_inplace(path, n):
    for i in range(n):
        with open(path, "wb") as f:          # truncate first: a window exists
            f.write(payload(i))
            f.flush()


def writer_replace(path, n):
    tmp = path + ".tmp"
    for i in range(n):
        with open(tmp, "wb") as f:
            f.write(payload(i))
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)                # rename(2): atomic


def run(mode, path):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(payload(0))
    w = mp.Process(target=writer_inplace if mode == "inplace" else writer_replace,
                   args=(path, ITERATIONS))
    w.start()
    reads = bad = 0
    while w.is_alive():
        try:
            with open(path, "rb") as f:
                data = f.read()
            reads += 1
            if json.loads(data).get("seq") is None:
                bad += 1
        except Exception:
            bad += 1
    w.join()
    # drain what is left so the last write is counted too
    return reads, bad


def main():
    print(f"payload={PAYLOAD_LEN} bytes  writer_iterations={ITERATIONS}  pid={os.getpid()}")
    base = tempfile.mkdtemp(prefix="whirl-atomic-probe-")
    print(f"work dir: {base}")
    for mode in ("inplace", "replace"):
        reads, bad = run(mode, os.path.join(base, f"state-{mode}.json"))
        pct = (100.0 * bad / reads) if reads else 0.0
        print(f"{mode:8s} reads={reads:9d} bad_reads={bad:7d} ({pct:.3f}% of reads saw "
              f"a truncated or invalid file)")
    shutil.rmtree(base, ignore_errors=True)


if __name__ == "__main__":
    main()
