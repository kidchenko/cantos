#!/usr/bin/env bash
#
# Builds Cantos for macOS or Linux. The counterpart to build.ps1.
#
# macOS:
#   ./build.sh                # test + release binary + Cantos.app (this Mac's arch)
#   ./build.sh --universal    # ... as one arm64 + x86_64 binary
#   ./build.sh --dmg          # ... then wrap Cantos.app in a .dmg
#   ./build.sh --run          # ... then launch it in the menu bar
#   ./build.sh --skip-tests
#
# Linux (X11; needs libgtk-3-dev libxdo-dev libayatana-appindicator3-dev
# to build):
#   ./build.sh                # test + release binary + dist/cantos-VERSION-linux-ARCH.tar.gz
#   ./build.sh --run          # ... then launch it in the tray
set -euo pipefail
cd "$(dirname "$0")"

run=0; dmg=0; universal=0; skip_tests=0
for arg in "$@"; do
  case "$arg" in
    --run) run=1 ;;
    --dmg) dmg=1 ;;
    --universal) universal=1 ;;
    --skip-tests) skip_tests=1 ;;
    *) echo "unknown option: $arg" >&2; exit 1 ;;
  esac
done

cyan() { printf '\n\033[36m==> %s\033[0m\n' "$1"; }
ok()   { printf '    \033[32m%s\033[0m\n' "$1"; }
warn() { printf '    \033[33m%s\033[0m\n' "$1"; }

cargo="$(command -v cargo || true)"
[ -z "$cargo" ] && [ -x "$HOME/.cargo/bin/cargo" ] && cargo="$HOME/.cargo/bin/cargo"
if [ -z "$cargo" ]; then
  echo "cargo not found: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
  exit 1
fi

# [package].version, not the first `version =` in the file.
version="$(awk '/^\[/{p=($0=="[package]")} p&&/^version[[:space:]]*=/{split($0,a,"\"");print a[2];exit}' Cargo.toml)"
[ -n "$version" ] || { echo 'could not read [package].version from Cargo.toml' >&2; exit 1; }

if [ "$skip_tests" -eq 0 ]; then
  cyan 'Running tests'
  "$cargo" test --quiet
  ok 'tests passed'
fi

if [ "$(uname -s)" = Linux ]; then
  cyan 'Building release binary'
  "$cargo" build --release
  exe=target/release/cantos
  ok "$(printf 'cantos %.2f MB' "$(echo "scale=2; $(stat -c%s "$exe")/1048576" | bc)")"

  cyan 'Packaging'
  arch="$(uname -m)"
  name="cantos-$version-linux-$arch"
  stage="$(mktemp -d)/$name"
  mkdir -p "$stage"
  cp "$exe" packaging/linux/install.sh packaging/linux/cantos.desktop LICENSE "$stage/"
  cp assets/icon-256.png "$stage/cantos.png"
  mkdir -p dist
  tar -C "$(dirname "$stage")" -czf "dist/$name.tar.gz" "$name"
  ok "dist/$name.tar.gz"
  ok "sha256  $(sha256sum "dist/$name.tar.gz" | cut -d' ' -f1)"

  if [ "$run" -eq 1 ]; then
    cyan 'Launching'
    pkill -x cantos 2>/dev/null || true
    nohup "$exe" --settings >/dev/null 2>&1 &
    ok 'running in the tray'
  fi
  exit 0
fi

cyan 'Building release binary'
if [ "$universal" -eq 1 ]; then
  rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
  "$cargo" build --release --target aarch64-apple-darwin
  "$cargo" build --release --target x86_64-apple-darwin
  mkdir -p target/universal
  exe=target/universal/cantos
  lipo -create -output "$exe" \
    target/aarch64-apple-darwin/release/cantos target/x86_64-apple-darwin/release/cantos
  ok "universal: $(lipo -archs "$exe")"
else
  "$cargo" build --release
  exe=target/release/cantos
fi
ok "$(printf 'cantos %.2f MB' "$(echo "scale=2; $(stat -f%z "$exe")/1048576" | bc)")"

cyan 'Assembling Cantos.app'
app=dist/Cantos.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$exe" "$app/Contents/MacOS/cantos"
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"

# A full iconset from the 1024 master, so Finder and the DMG get crisp art at
# every size rather than one upscaled bitmap.
if command -v iconutil >/dev/null && [ -f assets/icon-1024.png ]; then
  set_dir="$(mktemp -d)/Cantos.iconset"
  mkdir -p "$set_dir"
  for s in 16 32 128 256 512; do
    sips -z $s $s assets/icon-1024.png --out "$set_dir/icon_${s}x${s}.png" >/dev/null
    sips -z $((s*2)) $((s*2)) assets/icon-1024.png --out "$set_dir/icon_${s}x${s}@2x.png" >/dev/null
  done
  iconutil -c icns "$set_dir" -o "$app/Contents/Resources/Cantos.icns" && ok 'Cantos.icns'
else
  warn 'iconutil or assets/icon-1024.png missing; the app will use a default icon'
fi

# Signed with $CANTOS_SIGN_IDENTITY when set (an "Apple Development" or
# self-signed code-signing certificate), ad hoc otherwise. Real distribution
# wants a Developer ID + notarization (see README).
#
# macOS keys the Accessibility permission to the signature. With a
# certificate it survives rebuilds; ad hoc it is tied to this exact binary,
# so after a rebuild System Settings still shows Cantos switched on while
# macOS ignores it, and Cantos asks again however often it is allowed. So
# when an ad-hoc binary changes, its stale entry is cleared: the next prompt
# adds a fresh one.
if [ -n "${CANTOS_SIGN_IDENTITY:-}" ]; then
  codesign --force --sign "$CANTOS_SIGN_IDENTITY" "$app" >/dev/null 2>&1 \
    && ok "signed as $CANTOS_SIGN_IDENTITY" || warn 'codesign failed; unsigned'
elif codesign --force --sign - "$app" >/dev/null 2>&1; then
  ok 'ad-hoc signed'
  cdhash="$(codesign -dv --verbose=4 "$app" 2>&1 | sed -n 's/^CDHash=//p')"
  last=target/.cantos-cdhash
  if [ -n "$cdhash" ] && [ "$cdhash" != "$(cat "$last" 2>/dev/null)" ]; then
    tccutil reset Accessibility com.kidchenko.cantos >/dev/null 2>&1 \
      && warn 'new binary: Accessibility for Cantos reset; allow it again when asked'
    echo "$cdhash" >"$last"
  fi
else
  warn 'codesign failed; unsigned'
fi
ok "$app ($version)"

if [ "$dmg" -eq 1 ]; then
  cyan 'Building disk image'
  stage="$(mktemp -d)"
  cp -R "$app" "$stage/"
  # The drag-to-install target.
  ln -s /Applications "$stage/Applications"
  dmg_path="dist/Cantos-$version.dmg"
  rm -f "$dmg_path"
  hdiutil create -quiet -volname Cantos -srcfolder "$stage" -ov -format UDZO "$dmg_path"
  # Same name every release, for /releases/latest/download/Cantos.dmg.
  cp -f "$dmg_path" dist/Cantos.dmg
  ok "$dmg_path"
  ok "sha256  $(shasum -a 256 "$dmg_path" | cut -d' ' -f1)"
fi

if [ "$run" -eq 1 ]; then
  cyan 'Launching'
  pkill -x cantos 2>/dev/null || true
  open "$app"
  ok 'running in the menu bar'
fi
