#!/bin/sh
# macos.md [V9]: a sandboxed writer traps at launch on an ad-hoc signature.
#
# Builds wp_set.m with the com.apple.security.app-sandbox entitlement, ad-hoc signs it, and
# runs it (expected: Trace/BPT trap: 5), then builds and runs the same source with no
# entitlement as the control (expected: sets the wallpaper, exit 0).
#
#   sh sandbox_test.sh [image]
#
# Needs clang. The image defaults to whatever is live now, so the control write is a no-op
# for the user. The trap generates a crash report under ~/Library/Logs/DiagnosticReports;
# delete the ones this run creates.
#
# The entitlements file is a mktemp file named $TMPDIR/sandbox.entitlements.XXXXXX, and the run
# prints its path before codesign is given it. The `X`s are last because that is the one template
# both mktemp flavours accept: BSD mktemp refuses a template with anything after them and GNU
# mktemp requires at least three of them, so a template with the `X`s in the middle works on
# neither and the file's name no longer depends on which mktemp is first on PATH. The name was
# sandbox.XXXXXX.entitlements until 2026-09-29, when that template turned out to make BSD mktemp
# fail and codesign fail with it; a non-empty entitlements file is asserted before codesign now.
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
ENT=$(mktemp "${TMPDIR:-/tmp}/sandbox.entitlements.XXXXXX")
SB=/tmp/wp_set_sandbox
CTRL=/tmp/wp_set_control

cat > "$ENT" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>com.apple.security.app-sandbox</key><true/></dict></plist>
EOF

clang -fobjc-arc -framework AppKit -framework Foundation -framework CoreGraphics \
      -o "$SB" "$HERE/wp_set.m"

echo "entitlements: $ENT"
# The check that would have caught the defect above. A failed mktemp leaves $ENT empty and an empty
# --entitlements path does not stop this script: codesign fails with `Missing entitlements file
# path`, the linker's own ad-hoc signature stays on the binary, and the build that was meant to be
# sandboxed runs and sets a wallpaper instead of trapping. -s is false for an empty string as well
# as for an empty or missing file, so it catches both halves. Not `set -e`: the runs below are
# expected to exit non-zero, the sandboxed one with the trap.
[ -s "$ENT" ] || {
    echo "sandbox_test.sh: mktemp produced no entitlements file (path: ${ENT:-<empty>})" >&2
    echo "  nothing was signed and nothing was run" >&2
    exit 1
}

codesign --force --sign - --identifier com.whirl.sandboxprobe --entitlements "$ENT" "$SB"

image=${1:-$(/tmp/wp_probe 2>/dev/null | sed -n 's/^  desktopImageURL: //p' | head -1)}
echo "image: $image"
echo
echo "### sandboxed, ad-hoc signed: expect a trap before main()"
"$SB" "$image"
echo "exit=$?"
echo
echo "### log evidence (the AMFI lines come from the kernel and amfid, not the process)"
log show --last 2m --predicate 'eventMessage CONTAINS "wp_set_sandbox"' 2>/dev/null \
  | grep -i -E 'AMFI|amfid|adhoc|-423' || true
echo
echo "### control: same source, same ad-hoc signature, no entitlement"
clang -fobjc-arc -framework AppKit -framework Foundation -framework CoreGraphics \
      -o "$CTRL" "$HERE/wp_set.m"
codesign --force --sign - --identifier com.whirl.sandboxcontrol "$CTRL"
"$CTRL" "$image"
echo "exit=$?"
