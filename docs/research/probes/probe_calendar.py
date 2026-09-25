#!/usr/bin/env python3
"""Probe G/H: StartCalendarInterval behaviour when the firing moment passes.

G: job stays loaded across its firing minute  -> baseline, does it fire at all.
H: job is booted out just before its firing minute and bootstrapped again just
   after it. Question: does launchd remember a calendar firing that elapsed while
   the job was not registered (the "machine was off / agent not installed" case,
   as opposed to the documented sleep case).

Writes logs to $HOME/whirl-probe-{g,h}.log and prints timing evidence.
Run from docs/research/probes/ ; nothing is left loaded at the end.
"""
import os
import subprocess
import sys
import time
from datetime import datetime, timedelta

DOMAIN = f"gui/{os.getuid()}"
HERE = os.path.dirname(os.path.abspath(__file__))
HOME = os.path.expanduser("~")

TEMPLATE = """<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>com.whirl.research.probe{tag}</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/sh</string><string>-c</string>
        <string>printf '{tag} fire %s\\n' "$(date +%s.%N)" >> "{log}"</string>
    </array>
    <key>StartCalendarInterval</key>
    <dict>
        <key>Minute</key><integer>{minute}</integer>
    </dict>
    <key>StandardOutPath</key><string>/tmp/whirl-probe-{tag}.out</string>
    <key>StandardErrorPath</key><string>/tmp/whirl-probe-{tag}.err</string>
</dict>
</plist>
"""


def sh(*args):
    r = subprocess.run(args, capture_output=True, text=True)
    return r.returncode, (r.stdout + r.stderr).strip()


def write_plist(tag, minute):
    path = os.path.join(HERE, f"com.whirl.research.probe{tag}.plist")
    with open(path, "w") as fh:
        fh.write(TEMPLATE.format(tag=tag, minute=minute,
                                 log=os.path.join(HOME, f"whirl-probe-{tag}.log")))
    return path


def main():
    now = datetime.now()
    minute_g = (now.minute + 1) % 60
    minute_h = (now.minute + 2) % 60
    g_at = (now + timedelta(minutes=1)).replace(second=0, microsecond=0)
    h_at = (now + timedelta(minutes=2)).replace(second=0, microsecond=0)
    print(f"now={now.isoformat()}  G minute={minute_g} fires at {g_at.isoformat()}"
          f"  H minute={minute_h} fires at {h_at.isoformat()}", flush=True)

    g = write_plist("G", minute_g)
    h = write_plist("H", minute_h)

    print("bootstrap G:", sh("launchctl", "bootstrap", DOMAIN, g), flush=True)
    print("bootstrap H:", sh("launchctl", "bootstrap", DOMAIN, h), flush=True)
    time.sleep(2)
    rc, out = sh("launchctl", "print", f"{DOMAIN}/com.whirl.research.probeG")
    for line in out.splitlines():
        if any(k in line for k in ("state =", "next fire", "last exit", "pid =")):
            print("  G print:", line.strip(), flush=True)

    # keep H only until shortly before its firing minute, then drop it
    target = h_at - timedelta(seconds=8)
    while datetime.now() < target:
        time.sleep(1)
    print(f"bootout H at {datetime.now().isoformat()} (before its {h_at.isoformat()} fire)",
          sh("launchctl", "bootout", f"{DOMAIN}/com.whirl.research.probeH"), flush=True)

    # wait past H's firing minute, then bootstrap it again and let it settle
    target = h_at + timedelta(seconds=20)
    while datetime.now() < target:
        time.sleep(1)
    print(f"bootstrap H again at {datetime.now().isoformat()}", sh("launchctl", "bootstrap", DOMAIN, h), flush=True)
    time.sleep(75)

    for tag in ("G", "H"):
        rc, out = sh("launchctl", "print", f"{DOMAIN}/com.whirl.research.probe{tag}")
        for line in out.splitlines():
            if any(k in line for k in ("state =", "next fire", "last exit", "runs =")):
                print(f"  {tag} print:", line.strip(), flush=True)

    print("bootout G:", sh("launchctl", "bootout", f"{DOMAIN}/com.whirl.research.probeG"), flush=True)
    print("bootout H:", sh("launchctl", "bootout", f"{DOMAIN}/com.whirl.research.probeH"), flush=True)

    for tag in ("G", "H"):
        path = os.path.join(HOME, f"whirl-probe-{tag.lower()}.log")
        print(f"--- {tag} log ---", flush=True)
        if os.path.exists(path):
            print(open(path).read().strip(), flush=True)
        else:
            print("(no such file: the job never fired)", flush=True)


if __name__ == "__main__":
    sys.exit(main())
