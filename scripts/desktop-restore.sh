#!/bin/sh
#
# Put the live desktop picture back from a snapshot, and prove that it took.
#
#   scripts/desktop-restore.sh <file>
#
# macOS only, for the reason scripts/desktop-snapshot.sh gives: macOS is the one platform where
# this repository can set a real desktop image at all.
#
# What it does, in order:
#
#   1. reads the snapshot's Space node back out of the store, with
#      docs/research/probes/wallpaper_store.py, the one plist reader in this tree
#   2. asks the existing probe docs/research/probes/wp_probe.m which image is on screen, and
#      refuses to write unless the store says that image belongs to the Space node the snapshot
#      names. A write reaches the frontmost Space only (docs/research/macos.md section 3), so
#      restoring from another Space would repaint the wrong one and leave a trace there
#   3. sets the image with the harness setter, docs/research/probes/wp_set.m, built into a
#      scratch directory. The product's own setter is crates/whirl-worker/src/backend/macos.rs
#      (PR #14) and is not this script's business
#   4. reads the store back and exits non-zero unless that node holds the snapshot's image again
#      with a moved LastSet. The readback is the check: a restore that did not take has to fail
#      loudly, because a silent one leaves a test fixture as the live desktop picture
#
# A screenshot proves nothing here and is never taken. 'On screen' is only ever used to answer
# which Space you are on; what the desktop was left on is read from the store.
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
PROBES=$ROOT/docs/research/probes
PROBE=$PROBES/wallpaper_store.py

SCRATCH=${TMPDIR:-/tmp}
SCRATCH=${SCRATCH%/}/whirl-desktop-snapshot

usage() {
    echo "usage: ${0##*/} <file>" >&2
    echo "  <file>  a snapshot written by scripts/desktop-snapshot.sh" >&2
    exit 2
}

# build <name>: compile $PROBES/<name>.m into the scratch directory and print its path. The
# probes are build outputs, not committed files (docs/research/probes/README.md).
build() {
    src=$PROBES/$1.m
    out=$SCRATCH/$1
    if [ -x "$out" ] && [ "$out" -nt "$src" ]; then
        printf '%s\n' "$out"
        return 0
    fi
    [ -f "$src" ] || { echo "${0##*/}: $src is missing" >&2; exit 1; }
    command -v clang >/dev/null 2>&1 || {
        echo "${0##*/}: clang is needed to build $1 from $src" >&2
        echo "  install the Xcode command line tools: xcode-select --install" >&2
        exit 1
    }
    mkdir -p "$SCRATCH"
    echo "${0##*/}: building $out from $src" >&2
    clang -fobjc-arc -framework AppKit -framework Foundation -framework CoreGraphics \
        -o "$out" "$src" >&2
    printf '%s\n' "$out"
}

# value_of <key> <probe output>: the value of a key=value line from the store probe.
value_of() {
    printf '%s\n' "$2" | sed -n "s/^$1=//p"
}

# snapshot_field <key>: the value of a key=value line in the snapshot file.
snapshot_field() {
    sed -n "s/^$1=//p" "$file" | head -n 1
}

[ $# -eq 1 ] || usage
file=$1
case $file in
    /*) ;;
    *) file=$(pwd)/$file ;;
esac
[ -f "$file" ] || {
    echo "${0##*/}: no snapshot file $file" >&2
    exit 2
}
[ -f "$PROBE" ] || {
    echo "${0##*/}: $PROBE is missing; is this a checkout of the repository?" >&2
    exit 1
}

snap_space=$(snapshot_field space)
snap_url=$(snapshot_field url)
snap_path=$(snapshot_field path)
snap_taken=$(snapshot_field taken_utc)
[ -n "$snap_path" ] || {
    echo "${0##*/}: $file has no path= line" >&2
    echo "  this script restores a file written by scripts/desktop-snapshot.sh" >&2
    exit 2
}

# 1 and 2: which Space are we on, and is it the one the snapshot names?
wp_probe=$(build wp_probe)
screens=$("$wp_probe" | sed -n 's/^  desktopImageURL: //p')
if [ -z "$screens" ]; then
    echo "${0##*/}: $wp_probe reported no screen with a desktop image" >&2
    echo "  a real set needs a GUI session; this one cannot see the window server" >&2
    exit 1
fi
on_screen=$(printf '%s\n' "$screens" | sort -u)
if [ "$(printf '%s\n' "$on_screen" | grep -c .)" -ne 1 ]; then
    echo "${0##*/}: the screens are not all on one image, so the Space on screen is unknown:" >&2
    printf '%s\n' "$on_screen" | sed 's/^/  /' >&2
    exit 1
fi
if ! here=$(python3 "$PROBE" current --holds "$on_screen"); then
    echo "${0##*/}: the store does not name the Space node holding the image on screen" >&2
    echo "  the message above says why; nothing was written" >&2
    exit 1
fi
here_space=$(value_of space "$here")
here_path=$(value_of path "$here")
here_lastset=$(value_of lastset "$here")

if [ "$here_space" != "$snap_space" ]; then
    echo "${0##*/}: the frontmost Space is not the one the snapshot names." >&2
    echo "  snapshot  : Spaces[$snap_space], taken $snap_taken" >&2
    echo "  on screen : Spaces[$here_space]" >&2
    echo "  A write reaches the frontmost Space only, so restoring here would repaint the wrong" >&2
    echo "  Space and leave Spaces[$snap_space] holding whatever it holds now." >&2
    echo "  Switch to the Space the snapshot names and run this again." >&2
    exit 1
fi

if [ "$here_path" = "$snap_path" ]; then
    echo "already on the snapshot image, nothing to write"
    echo "space    : Spaces[$here_space]"
    echo "image    : $snap_url"
    echo "path     : $here_path"
    echo "LastSet  : $here_lastset   (UTC)"
    exit 0
fi

# 3. Put it back.
wp_set=$(build wp_set)
echo "setting  : $snap_path"
"$wp_set" "$snap_path"
# WallpaperAgent applies a write out of process; give it the beat the probes document before
# reading the store, or the readback reads the state that was already there.
sleep 2

# 4. Read the store back, and fail unless the snapshot's node holds its image again.
if ! after=$(python3 "$PROBE" current "$snap_space"); then
    echo "${0##*/}: Spaces[$snap_space] is no longer in the store, so the restore cannot be" >&2
    echo "  proven. The desktop may still be on the fixture; read the store by hand." >&2
    exit 1
fi
after_url=$(value_of url "$after")
after_path=$(value_of path "$after")
after_lastset=$(value_of lastset "$after")

if [ "$after_path" != "$snap_path" ]; then
    echo "${0##*/}: the restore did NOT take." >&2
    echo "  the snapshot wants : $snap_url" >&2
    echo "  the store now says : $after_url" >&2
    echo "  Spaces[$snap_space].Default.Desktop, LastSet $after_lastset (UTC)" >&2
    echo "  The desktop is still on the fixture. Do not report the check as done." >&2
    exit 1
fi
if [ "$after_lastset" = "$here_lastset" ]; then
    echo "${0##*/}: the store holds the snapshot image but LastSet did not move" >&2
    echo "  ($here_lastset before and after), so the write did not land on" >&2
    echo "  Spaces[$snap_space].Default.Desktop. The frontmost Space may have changed under it." >&2
    exit 1
fi

echo "restored : $after_url"
echo "path     : $after_path"
echo "space    : Spaces[$snap_space]"
echo "LastSet  : $here_lastset -> $after_lastset   (UTC)"
