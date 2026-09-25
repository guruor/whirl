#!/bin/sh
# macos.md [V5]: does the undocumented allSpaces option change which store nodes are written?
#
# Writes image A with the option and image B without it, snapshots the store after each,
# diffs both directions, then restores the image that was live before the run.
#
#   sh allspaces_test.sh [work-dir]
#
# Work dir defaults to a fresh mktemp dir; the snapshots it writes are named w0.plist,
# w1.plist and w2.plist inside it and are NOT committed (they carry this machine's
# wallpaper history). Needs /tmp/wp_set and /tmp/wp_probe built, and wallpaper_store.py:
#
#   clang -fobjc-arc -framework AppKit -framework Foundation -framework CoreGraphics \
#         -o /tmp/wp_set wp_set.m
#   clang -fobjc-arc -framework AppKit -framework Foundation -framework CoreGraphics \
#         -o /tmp/wp_probe wp_probe.m
#
# Expect "changed (choice or LastSet): 2" on the same two nodes in both directions, and
# LastUse churn on ~21 slots. Sleep between writes: a wallpaper write is asynchronous and
# a second call issued too soon can be dropped.
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${1:-$(mktemp -d "${TMPDIR:-/tmp}/allspaces.XXXXXX")}
STORE="$HOME/Library/Application Support/com.apple.wallpaper/Store/Index.plist"

[ -x /tmp/wp_set ] || { echo "build /tmp/wp_set first (see $HERE/README.md)" >&2; exit 2; }
[ -x /tmp/wp_probe ] || { echo "build /tmp/wp_probe first (see $HERE/README.md)" >&2; exit 2; }

before=$(/tmp/wp_probe | sed -n 's/^  desktopImageURL: //p' | head -1)
echo "work dir      : $OUT"
echo "live image    : $before"
cp "$STORE" "$OUT/w0.plist"

echo
echo "### write A with allSpaces"
/tmp/wp_set "/System/Library/Desktop Pictures/Mac Yellow.heic" allspaces
sleep 3
cp "$STORE" "$OUT/w1.plist"

echo
echo "### write B without allSpaces"
/tmp/wp_set "/System/Library/Desktop Pictures/Mac Pink.heic"
sleep 3
cp "$STORE" "$OUT/w2.plist"

echo
echo "############ diff w0 -> w1 (allSpaces) ############"
python3 "$HERE/wallpaper_store.py" diff "$OUT/w0.plist" "$OUT/w1.plist"
echo
echo "############ diff w1 -> w2 (no allSpaces) ############"
python3 "$HERE/wallpaper_store.py" diff "$OUT/w1.plist" "$OUT/w2.plist"

if [ -f "$before" ]; then
    echo
    echo "### restore the image that was live before the run"
    /tmp/wp_set "$before" || true
    sleep 2
    echo "readback: $(/tmp/wp_probe | sed -n 's/^  desktopImageURL: //p' | head -1)"
else
    echo
    echo "### not restoring: the previous image $before is gone from disk" >&2
fi
