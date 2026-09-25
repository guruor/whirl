#!/bin/sh
# macos.md [V7]: run the read probe from a launchd-submitted job.
#
#   launchctl submit -l com.whirl.probe4 -- /bin/sh probe-wrapper.sh
#   launchctl list com.whirl.probe4      # LimitLoadToSessionType = "Aqua"
#   launchctl remove com.whirl.probe4
#
# Builds /tmp/wp_probe from the committed source if it is missing, then runs it. launchd
# captures stdout; `launchctl list` reporting LastExitStatus = 0 plus the probe output is
# the evidence that a job in the Aqua session can read the wallpaper.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
[ -x /tmp/wp_probe ] || clang -fobjc-arc -framework AppKit -framework Foundation \
    -framework CoreGraphics -o /tmp/wp_probe "$HERE/wp_probe.m"
exec /tmp/wp_probe
