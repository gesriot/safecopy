#!/usr/bin/env bash
set -euo pipefail

APP_NAME="${APP_NAME:-HashTools}"
BINARY_NAME="${BINARY_NAME:-hashtools}"
BUNDLE_ID="${BUNDLE_ID:-com.hashtools.app}"
VERSION="${VERSION:-0.1.0}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEFAULT_BINARY_PATH="$ROOT_DIR/target/release/$BINARY_NAME"
BINARY_PATH="${BINARY_PATH:-$DEFAULT_BINARY_PATH}"
SKIP_BUILD="${SKIP_BUILD:-0}"
MACOS_DIR="$ROOT_DIR/macos"
ICON_PNG="${ICON_PNG:-$MACOS_DIR/icon.png}"
ICON_ROUNDED_PNG="${ICON_ROUNDED_PNG:-$MACOS_DIR/icon-rounded.png}"
ICON_RUNTIME_PNG="${ICON_RUNTIME_PNG:-$MACOS_DIR/icon-runtime.png}"
ICON_ICNS="${ICON_ICNS:-$MACOS_DIR/icon.icns}"
ICON_CORNER_RADIUS_RATIO="${ICON_CORNER_RADIUS_RATIO:-0.223}"
ICON_VISIBLE_SCALE="${ICON_VISIBLE_SCALE:-0.88}"
ICON_EDGE_THRESHOLD="${ICON_EDGE_THRESHOLD:-253}"
DIST_DIR="${DIST_DIR:-$ROOT_DIR/dist}"
APP_DIR="$DIST_DIR/$APP_NAME.app"
ZIP_PATH="$DIST_DIR/$APP_NAME.app.zip"
DMG_PATH="$DIST_DIR/$APP_NAME.dmg"

if [[ ! -f "$ICON_PNG" && ! -f "$ICON_ICNS" ]]; then
  echo "error: icon not found: $ICON_PNG or $ICON_ICNS" >&2
  exit 1
fi

mkdir -p "$DIST_DIR"
rm -rf "$APP_DIR" "$ZIP_PATH" "$DMG_PATH"

if [[ -f "$ICON_PNG" ]]; then
  python3 - "$ICON_PNG" "$ICON_ROUNDED_PNG" "$ICON_RUNTIME_PNG" "$ICON_ICNS" "$ICON_CORNER_RADIUS_RATIO" "$ICON_VISIBLE_SCALE" "$ICON_EDGE_THRESHOLD" <<'PY'
import sys
from pathlib import Path

try:
    from PIL import Image, ImageChops, ImageDraw
except ModuleNotFoundError:
    print("error: python3 Pillow package is required to convert icon.png to icon.icns", file=sys.stderr)
    sys.exit(1)

src = Path(sys.argv[1])
rounded_dst = Path(sys.argv[2])
runtime_dst = Path(sys.argv[3])
icns_dst = Path(sys.argv[4])
corner_radius_ratio = float(sys.argv[5])
visible_scale = float(sys.argv[6])
edge_threshold = int(sys.argv[7])

if not 0 < corner_radius_ratio < 0.5:
    print("error: ICON_CORNER_RADIUS_RATIO must be between 0 and 0.5", file=sys.stderr)
    sys.exit(1)
if not 0.5 <= visible_scale <= 1.0:
    print("error: ICON_VISIBLE_SCALE must be between 0.5 and 1.0", file=sys.stderr)
    sys.exit(1)
if not 0 <= edge_threshold <= 255:
    print("error: ICON_EDGE_THRESHOLD must be between 0 and 255", file=sys.stderr)
    sys.exit(1)

image = Image.open(src).convert("RGBA")

pixels = image.load()
xs = []
ys = []
for y in range(image.height):
    for x in range(image.width):
        r, g, b, a = pixels[x, y]
        if a and (a < 255 or min(r, g, b) < edge_threshold):
            xs.append(x)
            ys.append(y)

if xs and ys:
    left, top, right, bottom = min(xs), min(ys), max(xs) + 1, max(ys) + 1
else:
    left, top, right, bottom = 0, 0, image.width, image.height

crop_width = right - left
crop_height = bottom - top
crop_side = max(crop_width, crop_height)
center_x = (left + right) / 2
center_y = (top + bottom) / 2
left = int(round(center_x - crop_side / 2))
top = int(round(center_y - crop_side / 2))
right = left + crop_side
bottom = top + crop_side

if left < 0:
    right -= left
    left = 0
if top < 0:
    bottom -= top
    top = 0
if right > image.width:
    left -= right - image.width
    right = image.width
if bottom > image.height:
    top -= bottom - image.height
    bottom = image.height

left = max(left, 0)
top = max(top, 0)
cropped = image.crop((left, top, right, bottom))
side = max(cropped.size)
canvas = Image.new("RGBA", (side, side), (0, 0, 0, 0))
canvas.alpha_composite(cropped, ((side - cropped.width) // 2, (side - cropped.height) // 2))

scale = 4
mask_size = side * scale
mask = Image.new("L", (mask_size, mask_size), 0)
draw = ImageDraw.Draw(mask)
radius = int(round(side * corner_radius_ratio * scale))
draw.rounded_rectangle((0, 0, mask_size - 1, mask_size - 1), radius=radius, fill=255)
mask = mask.resize((side, side), Image.Resampling.LANCZOS)
canvas.putalpha(ImageChops.multiply(canvas.getchannel("A"), mask))

canvas.save(rounded_dst)
runtime_icon = Image.new("RGBA", (1024, 1024), (0, 0, 0, 0))
visible_side = int(round(1024 * visible_scale))
visible_icon = canvas.resize((visible_side, visible_side), Image.Resampling.LANCZOS)
runtime_icon.alpha_composite(
    visible_icon,
    ((1024 - visible_side) // 2, (1024 - visible_side) // 2),
)
runtime_icon.save(runtime_dst)
runtime_icon.save(
    icns_dst,
    format="ICNS",
    sizes=[(16, 16), (32, 32), (64, 64), (128, 128), (256, 256), (512, 512), (1024, 1024)],
)
PY
fi

if [[ "$SKIP_BUILD" != "1" && "$BINARY_PATH" == "$DEFAULT_BINARY_PATH" ]]; then
  cargo build --manifest-path "$ROOT_DIR/Cargo.toml" --release
fi

if [[ ! -x "$BINARY_PATH" ]]; then
  echo "error: built binary not found or not executable: $BINARY_PATH" >&2
  echo "Build it first, for example: cargo build --release" >&2
  exit 1
fi

mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"
cp "$BINARY_PATH" "$APP_DIR/Contents/MacOS/$APP_NAME"
chmod 755 "$APP_DIR/Contents/MacOS/$APP_NAME"

cp "$ICON_ICNS" "$APP_DIR/Contents/Resources/icon.icns"

cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDisplayName</key>
	<string>$APP_NAME</string>
	<key>CFBundleExecutable</key>
	<string>$APP_NAME</string>
	<key>CFBundleIconFile</key>
	<string>icon.icns</string>
	<key>CFBundleIdentifier</key>
	<string>$BUNDLE_ID</string>
	<key>CFBundleName</key>
	<string>$APP_NAME</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$VERSION</string>
	<key>CFBundleVersion</key>
	<string>$VERSION</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
PLIST

plutil -lint "$APP_DIR/Contents/Info.plist" >/dev/null
xattr -cr "$APP_DIR" 2>/dev/null || true

(
  cd "$DIST_DIR"
  COPYFILE_DISABLE=1 zip -qry -X "$APP_NAME.app.zip" "$APP_NAME.app"
)

DMG_STAGE="$(mktemp -d /tmp/hashtools-dmg.XXXXXX)"
cleanup() {
  rm -rf "$DMG_STAGE"
}
trap cleanup EXIT

cp -R "$APP_DIR" "$DMG_STAGE/"
ln -s /Applications "$DMG_STAGE/Applications"
COPYFILE_DISABLE=1 hdiutil create -volname "$APP_NAME" -srcfolder "$DMG_STAGE" -fs HFS+ -format UDZO -ov "$DMG_PATH" >/dev/null
hdiutil verify "$DMG_PATH" >/dev/null

echo "App: $APP_DIR"
echo "Zip: $ZIP_PATH"
echo "DMG: $DMG_PATH"
