#!/bin/sh
# macos.md [V5d]: the store is replaced, not edited in place.
#
# Snapshots the inode of Index.plist, performs one write with wp_set, snapshots the inode
# again, and lists the Store directory around the write to show no temp file is visible.
#
#   sh inode_test.sh [image]
#
# Image defaults to whatever is live now, so the run is a no-op for the user unless an
# explicit image is given. Needs /tmp/wp_set and /tmp/wp_probe built (see README.md).
# Expect the inode to change across one write.
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
STORE_DIR="$HOME/Library/Application Support/com.apple.wallpaper/Store"
STORE="$STORE_DIR/Index.plist"

[ -x /tmp/wp_set ] || { echo "build /tmp/wp_set first (see $HERE/README.md)" >&2; exit 2; }
[ -x /tmp/wp_probe ] || { echo "build /tmp/wp_probe first (see $HERE/README.md)" >&2; exit 2; }

image=${1:-$(/tmp/wp_probe | sed -n 's/^  desktopImageURL: //p' | head -1)}
echo "image: $image"
echo
echo "### before"
echo "inode: $(stat -f %i "$STORE")"
ls -la "$STORE_DIR"
echo
echo "### one write"
/tmp/wp_set "$image" || true
sleep 2
echo
echo "### after"
echo "inode: $(stat -f %i "$STORE")"
ls -la "$STORE_DIR"
