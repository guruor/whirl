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
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
ENT=$(mktemp "${TMPDIR:-/tmp}/sandbox.XXXXXX.entitlements")
SB=/tmp/wp_set_sandbox
CTRL=/tmp/wp_set_control

cat > "$ENT" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>com.apple.security.app-sandbox</key><true/></dict></plist>
EOF

clang -fobjc-arc -framework AppKit -framework Foundation -framework CoreGraphics \
      -o "$SB" "$HERE/wp_set.m"
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
