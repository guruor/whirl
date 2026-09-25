#!/usr/bin/env python3
"""Probe G2: does a StartCalendarInterval job fire at its minute boundary while
the job stays loaded (baseline), and what does launchctl print report about it.
Leaves nothing loaded."""
import os
import subprocess
import time
from datetime import datetime, timedelta

DOMAIN = f"gui/{os.getuid()}"
HERE = os.path.dirname(os.path.abspath(__file__))
HOME = os.path.expanduser("~")
LABEL = "com.whirl.research.probeG2"
TPL = """<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>%s</string>
    <key>ProgramArguments</key>
    <array><string>/bin/sh</string><string>-c</string>
    <string>printf 'G2 fire %%s\\n' "$(date +%%s.%%N)" >> "%s/whirl-probe-g2.log"</string></array>
    <key>StartCalendarInterval</key><dict><key>Minute</key><integer>%d</integer></dict>
    <key>StandardOutPath</key><string>/tmp/whirl-probe-g2.out</string>
    <key>StandardErrorPath</key><string>/tmp/whirl-probe-g2.err</string>
</dict>
</plist>
"""

now = datetime.now()
minute = (now.minute + 2) % 60
fire = (now + timedelta(minutes=2)).replace(second=0, microsecond=0)
plist = os.path.join(HERE, LABEL + ".plist")
with open(plist, "w") as fh:
    fh.write(TPL % (LABEL, HOME, minute))
print(f"now={now.isoformat()}  Minute={minute}  expected fire={fire.isoformat()}", flush=True)
print("bootstrap:", subprocess.run(["launchctl", "bootstrap", DOMAIN, plist],
                                   capture_output=True, text=True).stderr.strip(), flush=True)

while datetime.now() < fire + timedelta(seconds=25):
    time.sleep(2)

out = subprocess.run(["launchctl", "print", f"{DOMAIN}/{LABEL}"],
                     capture_output=True, text=True).stdout
print("launchctl print:", flush=True)
for line in out.splitlines():
    if any(k in line for k in ("state =", "runs =", "last exit", "pid =", "minimum runtime")):
        print("  " + line.strip(), flush=True)
print("bootout:", subprocess.run(["launchctl", "bootout", f"{DOMAIN}/{LABEL}"],
                                 capture_output=True, text=True).stderr.strip(), flush=True)
log = os.path.join(HOME, "whirl-probe-g2.log")
print("--- G2 log ---")
print(open(log).read().strip() if os.path.exists(log) else "(never fired)")
