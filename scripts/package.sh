#!/bin/bash
set -euo pipefail

VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
APP_NAME="Cuartel"
BUNDLE_DIR="target/${APP_NAME}.app"
DMG_NAME="target/${APP_NAME}-${VERSION}.dmg"

echo "building cuartel v${VERSION}..."
cargo build --release

echo "creating app bundle..."
rm -rf "${BUNDLE_DIR}"
mkdir -p "${BUNDLE_DIR}/Contents/MacOS"
mkdir -p "${BUNDLE_DIR}/Contents/Resources"

cp target/release/cuartel "${BUNDLE_DIR}/Contents/MacOS/"
cp Info.plist "${BUNDLE_DIR}/Contents/"

if [ -d assets ]; then
    cp -r assets/* "${BUNDLE_DIR}/Contents/Resources/" 2>/dev/null || true
fi

# Bundle the rivet sidecar
mkdir -p "${BUNDLE_DIR}/Contents/Resources/rivet"
cp rivet/package.json "${BUNDLE_DIR}/Contents/Resources/rivet/"
cp rivet/server.ts "${BUNDLE_DIR}/Contents/Resources/rivet/"
cp rivet/tsconfig.json "${BUNDLE_DIR}/Contents/Resources/rivet/"

echo "creating dmg..."
rm -f "${DMG_NAME}"
hdiutil create -volname "${APP_NAME}" -srcfolder "${BUNDLE_DIR}" -ov -format UDZO "${DMG_NAME}"

echo "done: ${DMG_NAME}"
