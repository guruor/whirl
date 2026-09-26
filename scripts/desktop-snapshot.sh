#!/bin/sh
#
# Snapshot the live desktop picture, so a real-hardware check can put it back.
#
#   scripts/desktop-snapshot.sh [-f] <file>
#
# macOS first, and macOS only: macOS is the one platform where this repository can set a real
# desktop image and read back what it set (crates/whirl-worker/src/backend/macos.rs, PR #14), so
# this script has no Windows or Linux half. It reads exactly two things:
#
#   the image on screen   the existing probe docs/research/probes/wp_probe.m, which calls
#                         desktopImageURLForScreen: the read macOS documents as reflecting the
#                         live Space's node (docs/research/macos.md section 2)
#   the store             docs/research/probes/wallpaper_store.py, the one plist reader in this
#                         tree. `current --holds <path>` turns the image that is on screen into
#                         the Space node holding it, which is where LastSet comes from.
#
# Why the two reads. The store does not record which Space is frontmost, and the LastUse-newest
# proxy the probes README used to name it is not reliable: measured 2026-09-26 07:00 UTC, the
# newest-LastUse Space node held wallhaven-28y8x9.jpg while the screen showed wh-5dzd11.jpg from
# a different node. Every in-use node carries the same write's LastUse within about a
# millisecond, so the newest of them is whichever the wallpaper agent touched last. So the
# on-screen image comes from the platform read, and the store says which node holds it.
#
# A snapshot is per-Space, because a wallpaper write reaches the frontmost Space only and the
# undocumented allSpaces option changes nothing (docs/research/macos.md section 3).
# scripts/desktop-restore.sh refuses to write unless the frontmost Space is the one this file
# names, so a snapshot cannot be used to repaint some other Space.
#
# The file is key=value lines, one per field, read with sed and never sourced: a path with spaces
# in it has to survive. -f overwrites an existing file.
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
PROBES=$ROOT/docs/research/probes
PROBE=$PROBES/wallpaper_store.py

SCRATCH=${TMPDIR:-/tmp}
SCRATCH=${SCRATCH%/}/whirl-desktop-snapshot

usage() {
    echo "usage: ${0##*/} [-f] <file>" >&2
    echo "  -f   overwrite <file> if it already exists" >&2
    exit 2
}

# build <name>: compile $PROBES/<name>.m into the scratch directory and print its path. The
# probes are build outputs, not committed files (docs/research/probes/README.md), and they are
# the test harness: the product's own setter is PR #14's business, not this script's.
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

# value_of <key> <probe output>: the value of the key=value line the store probe printed.
value_of() {
    printf '%s\n' "$2" | sed -n "s/^$1=//p"
}

force=0
file=
for arg in "$@"; do
    case $arg in
        -f) force=1 ;;
        -*) usage ;;
        *)  [ -z "$file" ] || usage
            file=$arg ;;
    esac
done
[ -n "$file" ] || usage

case $file in
    /*) ;;
    *) file=$(pwd)/$file ;;
esac

[ -f "$PROBE" ] || {
    echo "${0##*/}: $PROBE is missing; is this a checkout of the repository?" >&2
    exit 1
}
if [ -e "$file" ] && [ "$force" -ne 1 ]; then
    echo "${0##*/}: $file exists; pass -f to overwrite it" >&2
    exit 1
fi

# 1. What is on screen. One image, or this is not a state a snapshot can name.
wp_probe=$(build wp_probe)
screens=$("$wp_probe" | sed -n 's/^  desktopImageURL: //p')
if [ -z "$screens" ]; then
    echo "${0##*/}: $wp_probe reported no screen with a desktop image" >&2
    echo "  a real set needs a GUI session; this one cannot see the window server" >&2
    exit 1
fi
on_screen=$(printf '%s\n' "$screens" | sort -u)
if [ "$(printf '%s\n' "$on_screen" | grep -c .)" -ne 1 ]; then
    echo "${0##*/}: the screens are not all on one image, so there is no single state to save:" >&2
    printf '%s\n' "$on_screen" | sed 's/^/  /' >&2
    exit 1
fi

# 2. Which Space node holds it, and when that node was last written.
if ! node=$(python3 "$PROBE" current --holds "$on_screen"); then
    echo "${0##*/}: the store does not name a Space node holding the image on screen" >&2
    echo "  the message above says why; nothing was written" >&2
    exit 1
fi
space=$(value_of space "$node")
url=$(value_of url "$node")
path=$(value_of path "$node")
lastset=$(value_of lastset "$node")

if [ ! -f "$path" ]; then
    echo "${0##*/}: the live image is not on disk, so no restore could put it back:" >&2
    echo "  $path" >&2
    echo "  the store still names it: $url" >&2
    echo "  Setting a snapshot that cannot be set again would be a promise this file cannot" >&2
    echo "  keep. Put the picture you want to keep on this Space, then snapshot that; nothing" >&2
    echo "  was written." >&2
    exit 1
fi

# 3. Write it down. Values are raw, one per line, no quoting: read with sed, not sourced.
{
    echo "# whirl desktop snapshot: the frontmost Space, on macOS."
    echo "# Written by scripts/desktop-snapshot.sh; put it back with"
    echo "#   scripts/desktop-restore.sh $file"
    echo "# key=value lines, one per field. Values are raw: read with sed, never sourced."
    echo "taken_utc=$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    echo "space=$space"
    echo "url=$url"
    echo "path=$path"
    echo "lastset=$lastset"
} > "$file"

echo "snapshot : $file"
echo "screen   : $(printf '%s\n' "$on_screen")"
echo "space    : Spaces[$space]"
echo "image    : $url"
echo "path     : $path"
echo "LastSet  : $lastset   (UTC)"
