#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
[[ "$(uname -s)" == Darwin ]] || { echo 'Run this script on macOS.' >&2; exit 1; }
case "${1:-$(uname -m)}" in
  arm64|aarch64) arch=arm64; target=aarch64-apple-darwin ;;
  x64|x86_64) arch=x64; target=x86_64-apple-darwin ;;
  *) echo 'Usage: bash scripts/build-macos.sh [arm64|x64]' >&2; exit 1 ;;
esac
export MACOSX_DEPLOYMENT_TARGET=12.0
version=$(sed -n 's/^version = "\([0-9.]*\)"/\1/p' Cargo.toml | head -1)
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
rustup target add "$target"
cargo build --locked --release -p rshell --target "$target"
stage="dist/RShell-$version-macos-$arch"
[[ ! -e "$stage" && ! -e "$stage.zip" ]] || { echo "Output already exists: $stage" >&2; exit 1; }
app="$stage/RShell.app/Contents"
mkdir -p "$app/MacOS" "$app/Resources/licenses"
build="${CARGO_TARGET_DIR:-target}/$target/release"
cp "$build/rshell" "$build/rshell-cli" "$app/MacOS/"
cp LICENSE "$app/Resources/"
cp crates/rshell-app/assets/fonts/LICENSE.txt "$app/Resources/licenses/Geist-OFL.txt"
cp crates/rshell-app/assets/lucide/LICENSE "$app/Resources/licenses/Lucide.txt"
cp docs/MACOS.md "$stage/README.md"
cat > "$app/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>RShell</string>
<key>CFBundleDisplayName</key><string>RShell</string>
<key>CFBundleIdentifier</key><string>com.RShell.RShell</string>
<key>CFBundleExecutable</key><string>rshell</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$version</string>
<key>CFBundleVersion</key><string>$version</string>
<key>LSMinimumSystemVersion</key><string>12.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
EOF
plutil -lint "$app/Info.plist"
# Ad-hoc signing allows local Apple Silicon execution; this is not Developer ID/notarization.
codesign --force --sign - "$app/MacOS/rshell-cli"
codesign --force --sign - "$stage/RShell.app"
codesign --verify --deep --strict "$stage/RShell.app"
ditto -c -k --sequesterRsrc --keepParent "$stage" "$stage.zip"
(cd dist && shasum -a 256 "$(basename "$stage").zip" > "$(basename "$stage").zip.sha256")
echo "Package: $stage.zip"
