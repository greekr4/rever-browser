#!/bin/sh
# Builds "dist/Rever Browser.app" and a .dmg of it.
#
#   scripts/package-macos.sh           the release build (a full Servo compile the first time)
#   scripts/package-macos.sh --debug   the debug build, to check the bundle quickly
#
# The app is signed ad hoc: it runs on this Mac, and on another one after the user
# allows it in System Settings. A Developer ID signature and notarization are not done here.
set -eu
cd "$(dirname "$0")/.."

profile=release
if [ "${1:-}" = "--debug" ]; then
  profile=debug
  cargo build -p rever
else
  cargo build -p rever --release
fi

name="Rever Browser"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' crates/rever/Cargo.toml | head -1)
app="dist/$name.app"
binary="$app/Contents/MacOS/rever"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$app/Contents/Frameworks"
cp "target/$profile/rever" "$binary"
cp resources/icon.icns "$app/Contents/Resources/icon.icns"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$name</string>
  <key>CFBundleDisplayName</key><string>$name</string>
  <key>CFBundleIdentifier</key><string>io.greekr4.reverbrowser</string>
  <key>CFBundleExecutable</key><string>rever</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# Libraries from Homebrew (dav1d) are not on another Mac: they travel inside the app.
otool -L "$binary" | awk 'NR > 1 { print $1 }' | grep -v -e '^/System/' -e '^/usr/lib/' |
  while read -r library; do
    file=$(basename "$library")
    cp "$library" "$app/Contents/Frameworks/$file"
    chmod u+w "$app/Contents/Frameworks/$file"
    install_name_tool -id "@executable_path/../Frameworks/$file" "$app/Contents/Frameworks/$file"
    install_name_tool -change "$library" "@executable_path/../Frameworks/$file" "$binary"
    # One level is carried; a library that needs another one outside the system stops here.
    if otool -L "$app/Contents/Frameworks/$file" | awk 'NR > 2 { print $1 }' |
      grep -v -e '^/System/' -e '^/usr/lib/' -e '^@'; then
      echo "package-macos: $file needs the libraries above, which are not bundled" >&2
      exit 1
    fi
  done

# Changing the load paths broke the signatures; Apple Silicon runs nothing unsigned.
codesign --force --deep --sign - "$app"

dmg="dist/rever-browser-$version-$(uname -m).dmg"
rm -f "$dmg"
hdiutil create -quiet -volname "$name" -srcfolder "$app" -format UDZO "$dmg"
echo "$app"
echo "$dmg ($(du -h "$dmg" | cut -f1))"
