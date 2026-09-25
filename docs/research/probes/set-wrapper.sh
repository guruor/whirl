#!/bin/sh
# macos.md [V7]: run the write probe from a launchd-submitted job.
#
#   launchctl submit -l com.whirl.probe6 -- /bin/sh set-wrapper.sh [image]
#
# With no argument it re-sets the image that is currently live. Note what that does and
# does not prove: re-writing the image that is already set is a no-op in the store (no
# `LastSet` bump), so pass an image that differs from the live one when you want to see the
# write register. [V7] saw the bump because the image it wrote was not the one on screen.
# Builds /tmp/wp_set and /tmp/wp_probe from the committed source if they are missing.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
[ -x /tmp/wp_probe ] || clang -fobjc-arc -framework AppKit -framework Foundation \
    -framework CoreGraphics -o /tmp/wp_probe "$HERE/wp_probe.m"
[ -x /tmp/wp_set ] || clang -fobjc-arc -framework AppKit -framework Foundation \
    -framework CoreGraphics -o /tmp/wp_set "$HERE/wp_set.m"
image=${1:-$(/tmp/wp_probe | sed -n 's/^  desktopImageURL: //p' | head -1)}
[ -n "$image" ] || { echo "no image argument and no live image" >&2; exit 2; }
exec /tmp/wp_set "$image"
