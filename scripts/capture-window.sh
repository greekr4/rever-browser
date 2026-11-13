#!/bin/sh
# Capture one app's own window to a PNG (macOS): scripts/capture-window.sh <process-name> <out.png>
# Only that window is captured, never the rest of the screen. Used to look at GPUI previews.
set -e
name="$1"; out="$2"
id=$(swift - "$name" <<'SWIFT'
import CoreGraphics
import Foundation
let name = CommandLine.arguments[1]
let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
for w in list {
    if let owner = w[kCGWindowOwnerName as String] as? String, owner == name,
       let id = w[kCGWindowNumber as String] as? Int, (w[kCGWindowLayer as String] as? Int) == 0 {
        print(id); break
    }
}
SWIFT
)
[ -n "$id" ] || { echo "no window owned by '$name'" >&2; exit 1; }
screencapture -x -o -l "$id" "$out"
