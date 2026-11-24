#!/usr/bin/env bash
# Builds wawi2 in release and assembles Wawi.app (macOS, Apple Silicon).
#
#   scripts/bundle-mac.sh              # deja dist/Wawi.app
#   WAWI_APP_OUT=/Applications scripts/bundle-mac.sh
#
# Variables:
#   WAWI_APP_OUT        carpeta donde queda Wawi.app (por defecto dist/)
#   CARGO_TARGET_DIR    carpeta de compilación (por defecto target/)
#   WAWI_SIGN_IDENTITY  identidad de firma (por defecto "-", ad-hoc). Con un
#                       certificado propio (por ejemplo uno autofirmado de
#                       Acceso a Llaveros), los permisos sobreviven a cada
#                       recompilación.
#
# Ad-hoc signing only: for wide distribution Wawi.app needs a Developer ID
# certificate, the hardened runtime (with the audio-input entitlement) and
# notarization. That is out of scope here.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${WAWI_APP_OUT:-$ROOT/dist}"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
IDENTITY="${WAWI_SIGN_IDENTITY:--}"
BUNDLE_ID="io.github.msenatori.wawi"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
APP="$OUT/Wawi.app"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "Wawi.app solo se arma en macOS." >&2
  exit 1
fi

echo "1/5 Compilando (release)…"
cargo build --release --locked -p wawi-app --manifest-path "$ROOT/Cargo.toml"
BIN="$TARGET_DIR/release/wawi2"
[[ -x "$BIN" ]] || { echo "No encuentro $BIN" >&2; exit 1; }

echo "2/5 Armando ${APP}…"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/wawi2"
# The body looks for its pets in Contents/Resources/mascotas.
cp -R "$ROOT/assets/mascotas" "$APP/Contents/Resources/mascotas"
cp "$ROOT/LICENSE" "$ROOT/NOTICE" "$APP/Contents/Resources/"

echo "3/5 Ícono, con el dibujo de Wawi…"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
ICONSET="$WORK/Wawi.iconset"
mkdir -p "$ICONSET"
sips -s format png "$ROOT/assets/mascotas/wawi/wawi_dibujo.jpg" --out "$WORK/base.png" >/dev/null
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$WORK/base.png" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" "$WORK/base.png" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/Wawi.icns"

echo "4/5 Info.plist…"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>es</string>
  <key>CFBundleDisplayName</key>
  <string>Wawi</string>
  <key>CFBundleExecutable</key>
  <string>wawi2</string>
  <key>CFBundleIconFile</key>
  <string>Wawi</string>
  <key>CFBundleIdentifier</key>
  <string>$BUNDLE_ID</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>Wawi</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>$VERSION</string>
  <key>CFBundleVersion</key>
  <string>$VERSION</string>
  <key>LSApplicationCategoryType</key>
  <string>public.app-category.productivity</string>
  <key>LSMinimumSystemVersion</key>
  <string>13.0</string>
  <key>LSUIElement</key>
  <true/>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSHumanReadableCopyright</key>
  <string>Código: Apache-2.0. Dibujos de Wawi: CC BY-NC 4.0.</string>
  <key>NSMicrophoneUsageDescription</key>
  <string>Wawi escucha el micrófono solo mientras mantenés apretada la tecla ⌘ derecha, para entender lo que le decís. El audio se procesa en tu Mac.</string>
  <key>NSAppleEventsUsageDescription</key>
  <string>Wawi usa esto solo si un plugin tuyo le pide manejar otra app.</string>
</dict>
</plist>
PLIST
plutil -lint "$APP/Contents/Info.plist"

echo "5/5 Firmando ($([[ "$IDENTITY" == "-" ]] && echo ad-hoc || echo "$IDENTITY"))…"
# A designated requirement by bundle id, so a rebuild keeps the same identity
# for macOS's privacy permissions (TCC) instead of a per-build hash.
codesign --force --sign "$IDENTITY" --identifier "$BUNDLE_ID" \
  -r="designated => identifier \"$BUNDLE_ID\"" "$APP"
codesign --verify --strict "$APP"

echo
echo "Listo: $APP"
echo "Primero, si no lo hiciste:  \"$APP/Contents/MacOS/wawi2\" setup"
echo "Después abrila (doble clic, o: open \"$APP\")."
