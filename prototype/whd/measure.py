#!/usr/bin/env python3
"""Measure a running whd daemon: idle RSS, peak RSS during rotations, post-rotation RSS."""
import json
import subprocess
import sys
import threading
import time
import urllib.request

import os

BASE = os.environ.get("WHD_BASE", "http://127.0.0.1:8791")
ITERS = int(sys.argv[1]) if len(sys.argv) > 1 else 12
LABEL = sys.argv[2] if len(sys.argv) > 2 else "daemon"


def get(path):
    return json.load(urllib.request.urlopen(BASE + path, timeout=180))


def post(path):
    return json.load(urllib.request.urlopen(urllib.request.Request(BASE + path, method="POST"), timeout=180))


pid = get("/status")["pid"]

samples = []
stop = threading.Event()


def poll():
    while not stop.is_set():
        out = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)],
                             capture_output=True, text=True).stdout.strip()
        if out:
            samples.append(int(out) / 1024.0)
        time.sleep(0.05)


threading.Thread(target=poll, daemon=True).start()
time.sleep(1.5)
idle = get("/status")["rss_mb"]
print(f"{LABEL}: idle {idle:.1f} MB  pid={pid}  inproc={get('/status')['inproc']}")

for i in range(ITERS):
    try:
        body = post("/next")
        res = str(body.get("result", ""))
    except Exception as e:
        res = f"ERROR {e}"
    st = get("/status")
    print(f"  rot {i+1:2d}  post-rotation {st['rss_mb']:6.1f} MB  goroutines={st['goroutines']:2d}  | {res[:64]}")

stop.set()
time.sleep(0.3)
final = get("/status")
print(f"{LABEL}: peak sampled during run {max(samples):.1f} MB | final {final['rss_mb']:.1f} MB | "
      f"cache_files={final['cache_files']} rotations={final['rotations']}")
