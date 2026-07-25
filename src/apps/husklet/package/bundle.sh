#!/usr/bin/env bash
# Assemble a self-contained, ad-hoc-signed hl.app on macOS.
#
# Must run inside the GTK dev shell so the GTK build/runtime data and tools are on PATH:
#   nix develop "path:$PWD/nix" --command tools/bundle.sh
# (the Makefile `app` target does this for you).
#
# Produces Husklet.app with:
#   Contents/MacOS/husklet                    the workspace application
#   Contents/Resources/hl-daemon              the container daemon with its engine linked in
#   Contents/Frameworks/*.dylib               the relocated GTK dylib graph (dylibbundler)
#   Contents/Frameworks/gdk-pixbuf-.../       svg+png loaders with a relative loaders.cache
#   Contents/Resources/glib-2.0/schemas/      compiled gschemas
#   Contents/Resources/icons/                 Adwaita + hicolor icon themes
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
VERSION="${1:-0.1.0}"
export HL_VERSION="${HL_VERSION:-$VERSION}"
BUILD_TARGET="${CARGO_TARGET_DIR:-$ROOT/target-macos}"
BUNDLE_TARGET="${HL_BUNDLE_TARGET:-$ROOT/target}"
FINAL_APP="$BUNDLE_TARGET/Husklet.app"
APP="$BUNDLE_TARGET/.Husklet.app.build.$$"
C="$APP/Contents"; MACOS="$C/MacOS"; RES="$C/Resources"; FW="$C/Frameworks"
ENT="${HL_ENGINE_ENTITLEMENTS:-$ROOT/src/apps/husklet/package/engine.entitlements}"
trap 'rm -rf "$APP"' EXIT

log() { printf '\033[1;34m[bundle]\033[0m %s\n' "$*"; }
die() { printf '\033[1;31m[bundle] %s\033[0m\n' "$*" >&2; exit 1; }

[ "$(uname)" = "Darwin" ] || die "must run on macOS"
[ -n "${HL_GTK4:-}" ] || die "run inside the nix dev shell: nix develop \"path:$ROOT/nix\" --command tools/bundle.sh"
command -v dylibbundler >/dev/null || die "dylibbundler not found (nix dev shell)"

# 1. Release product binaries. Cargo links the native archive shipped by the selected `hl-engine` crate,
# keeping the Rust API and native ABI on one published version.
log "building release husklet and hl-daemon"
( cd "$ROOT" && export RUSTFLAGS="-L native=$HL_LIBXKBCOMMON/lib ${RUSTFLAGS:-}" && \
    cargo build --release -p hl-daemon && \
    cargo build --release -p husklet --features gui --bin husklet )
[ -f "$ENT" ] || die "engine entitlements missing at $ENT"

# 2. Skeleton. Assemble beside the installed bundle so a running or newly
# launched application never observes a partially relocated dependency graph.
log "laying out bundle skeleton"
chmod -R u+w "$APP" 2>/dev/null || true
rm -rf "$APP"
mkdir -p "$MACOS" "$RES" "$FW"
cp "$BUILD_TARGET/release/husklet" "$MACOS/husklet"
cp "$BUILD_TARGET/release/hl-daemon" "$RES/hl-daemon"
printf 'APPL????' > "$C/PkgInfo"
sed "s/@VERSION@/$VERSION/g" "$ROOT/src/apps/husklet/package/Info.plist.in" > "$C/Info.plist"
[ -f "$ROOT/src/apps/husklet/package/husklet.icns" ] && cp "$ROOT/src/apps/husklet/package/husklet.icns" "$RES/husklet.icns" || true
[ -f "$ROOT/assets/logo.png" ] && cp "$ROOT/assets/logo.png" "$RES/logo.png" || true # onboarding logo
[ -d "$ROOT/assets/images" ] && cp -R "$ROOT/assets/images" "$RES/images" || true # bundled starter images

# Linux guest drivers are part of the product, not mutable user state. The Rust surface crates stage a
# freshly cross-linked aarch64 set while building Husklet; fail closed if any required object is absent.
DRIVER_STAGE="${HOME:?HOME is required}/.hl"
DRIVER_DEST="$RES/drivers"
DRIVER_FILES=(
  gl/aarch64/libEGL.so.1
  gl/aarch64/libGLESv2.so.2
  gl/aarch64/libwayland-egl.so.1
  vulkan/aarch64/libvk_hl.so.1
  vulkan/aarch64/icd.json
  cuda/aarch64/libcuda.so.1
  cuda/aarch64/libcudart.so.1
  nvml/aarch64/libnvidia-ml.so.1
)
for driver in "${DRIVER_FILES[@]}"; do
  [ -f "$DRIVER_STAGE/$driver" ] || die "required guest driver missing: $DRIVER_STAGE/$driver"
done
READELF="${HL_AARCH64_LINUX_CC%gcc}readelf"
[ -x "$READELF" ] || die "aarch64 Linux readelf missing beside $HL_AARCH64_LINUX_CC"
ELF_NM="${HL_AARCH64_LINUX_CC%gcc}nm"
[ -x "$ELF_NM" ] || die "aarch64 Linux nm missing beside $HL_AARCH64_LINUX_CC"
DRIVER_SONAMES=(
  gl/aarch64/libEGL.so.1:libEGL.so.1
  gl/aarch64/libGLESv2.so.2:libGLESv2.so.2
  gl/aarch64/libwayland-egl.so.1:libwayland-egl.so.1
  vulkan/aarch64/libvk_hl.so.1:libvk_hl.so.1
  cuda/aarch64/libcuda.so.1:libcuda.so.1
  cuda/aarch64/libcudart.so.1:libcudart.so.1
  nvml/aarch64/libnvidia-ml.so.1:libnvidia-ml.so.1
)
for entry in "${DRIVER_SONAMES[@]}"; do
  driver="${entry%%:*}"; soname="${entry#*:}"; artifact="$DRIVER_STAGE/$driver"
  file "$artifact" | grep -q 'ELF 64-bit.*ARM aarch64' || die "guest driver is not aarch64 ELF: $artifact"
  "$READELF" -d "$artifact" | grep -Fq "Library soname: [$soname]" || \
    die "guest driver has wrong SONAME (expected $soname): $artifact"
done
"$READELF" -d "$DRIVER_STAGE/gl/aarch64/libGLESv2.so.2" | \
  grep -Fq 'Shared library: [libEGL.so.1]' || die "libGLESv2 does not bind the shared EGL state owner"
for api in gl egl; do
  if [ "$api" = gl ]; then artifact="$DRIVER_STAGE/gl/aarch64/libGLESv2.so.2"; else artifact="$DRIVER_STAGE/gl/aarch64/libEGL.so.1"; fi
  golden="$ROOT/src/surface/hl-gl/shim/egl/tests/golden/abi_symbols_${api}.txt"
  actual="$(mktemp -t husklet-${api}-exports.XXXXXX)"
  "$ELF_NM" -D --defined-only "$artifact" | awk '{print $3}' | grep "^${api}" | LC_ALL=C sort -u > "$actual" || true
  diff -u "$golden" "$actual" >/dev/null || { rm -f "$actual"; die "$artifact does not export the complete ${api} ABI"; }
  rm -f "$actual"
done
log "staging Linux guest drivers"
mkdir -p "$DRIVER_DEST"
for family in gl vulkan cuda nvml; do
  cp -R "$DRIVER_STAGE/$family" "$DRIVER_DEST/$family"
done

# The compositor is a library linked into the Husklet executable. Husklet re-executes itself with the
# private `__compositor` operation so AppKit presentation starts on that process's main thread. There is no
# standalone compositor binary to package; relocating Husklet's dylib graph below includes libxkbcommon.
[ -n "${HL_LIBXKBCOMMON:-}" ] || die "HL_LIBXKBCOMMON is required by the embedded compositor"

# 3. Stage gdk-pixbuf loaders (png from gdk-pixbuf, svg from librsvg) with a RELATIVE cache.
#    They live under Resources/ (NOT Frameworks/) so Frameworks stays a flat set of dylibs —
#    codesign refuses to seal a bundle whose Frameworks holds bare non-code directories. Their
#    own dependency paths are @executable_path-relative, so they still find Frameworks/*.dylib.
log "staging gdk-pixbuf loaders"
PIXVER="$(basename "$(ls -d "$HL_GDK_PIXBUF"/lib/gdk-pixbuf-2.0/*/ | head -1)")"
DEST_LOADERS="$RES/lib/gdk-pixbuf-2.0/$PIXVER/loaders"
mkdir -p "$DEST_LOADERS"
# Copy every gdk-pixbuf loader present (png/jpeg are built into core, so none is required for our
# UI; we ship what exists for completeness) plus librsvg's svg loader if this build provides one.
cp -L "$HL_GDK_PIXBUF"/lib/gdk-pixbuf-2.0/"$PIXVER"/loaders/*.so "$DEST_LOADERS"/ 2>/dev/null || true
find "$HL_LIBRSVG" -name libpixbufloader-svg.so -exec cp -L {} "$DEST_LOADERS"/ \; 2>/dev/null || true

shopt -s nullglob
LOADER_SOS=( "$DEST_LOADERS"/*.so )
shopt -u nullglob

# Generate metadata while the copied loaders still reference their valid Nix dependencies. Once
# relocated, query-loaders itself resolves `@executable_path` from the Nix tool rather than Husklet,
# so querying at that point rejects otherwise valid app-relative plugins and writes an empty cache.
if [ ${#LOADER_SOS[@]} -gt 0 ]; then
  GDK_PIXBUF_MODULEDIR="$DEST_LOADERS" gdk-pixbuf-query-loaders "${LOADER_SOS[@]}" \
    | sed -E "s#\"$DEST_LOADERS/#\"#g" > "$DEST_LOADERS/loaders.cache"
  grep -q '^"' "$DEST_LOADERS/loaders.cache" || die "gdk-pixbuf loader cache is empty"
else
  : > "$DEST_LOADERS/loaders.cache"
fi

# 4. Relocate the dylib graph: Husklet + each loader .so (+ hl-compositor when built) ->
#    Contents/Frameworks. Adding the compositor binaries here pulls libxkbcommon (and any other non-system
#    dylib the Smithay renderer links) into Frameworks with @executable_path/../Frameworks install names,
#    which — for a binary in Resources/ — resolves to Contents/Frameworks. That is what lets an end-user
#    hl.app launch the compositor (readiness Gap 4).
log "relocating dylibs (dylibbundler)"
XARGS=( -x "$MACOS/husklet" -x "$RES/hl-daemon" )
for so in "${LOADER_SOS[@]}"; do XARGS+=( -x "$so" ); done
dylibbundler -of -cd -b -d "$FW" -p '@executable_path/../Frameworks' "${XARGS[@]}" >/dev/null

# 5. Compile GSettings schemas (GTK aborts at startup without them).
log "compiling gsettings schemas"
SCHEMA_DEST="$RES/glib-2.0/schemas"
mkdir -p "$SCHEMA_DEST"
# nixpkgs installs schemas below `share/gsettings-schemas/<package>/glib-2.0/schemas`, while other
# distributions install them directly below `share/glib-2.0/schemas`. Discover both layouts.
while IFS= read -r -d '' schema; do cp -L "$schema" "$SCHEMA_DEST"/; done < <(
  find "$HL_GTK4/share" "$HL_GSETTINGS_SCHEMAS/share" \
    -path '*/glib-2.0/schemas/*.xml' -type f -print0
)
glib-compile-schemas "$SCHEMA_DEST" >/dev/null
[ -s "$SCHEMA_DEST/gschemas.compiled" ] || die "GSettings schema bundle is empty"
gsettings --schemadir "$SCHEMA_DEST" list-schemas \
  | grep -qx 'org.gtk.gtk4.Settings.ColorChooser' \
  || die "GTK color chooser schema is missing"

# 6. Icon themes (Adwaita symbolic icons used by the toolbar + hicolor fallback).
log "staging icon themes"
mkdir -p "$RES/icons"
cp -RL "$HL_ADWAITA_ICONS"/share/icons/Adwaita "$RES/icons"/ 2>/dev/null || true
cp -RL "$HL_HICOLOR_ICONS"/share/icons/hicolor "$RES/icons"/ 2>/dev/null || true
command -v gtk4-update-icon-cache >/dev/null && gtk4-update-icon-cache -q -f -t "$RES/icons/Adwaita" 2>/dev/null || true

# 7. Fontconfig.
mkdir -p "$RES/fontconfig"
cp "$ROOT/src/apps/husklet/package/fonts.conf" "$RES/fontconfig/fonts.conf"

# 7b. Resolve the libiconv name-collision. nixpkgs ships TWO different libiconv.2.dylib: Apple's (exports
# _iconv, used by glib/gtk) and GNU's (exports _libiconv, used by libidn2/libunistring). dylibbundler
# collapses them into one file, so whichever consumer needs the other's symbols crashes at dyld time.
# Fix: keep Apple's as libiconv.2.dylib, and give the GNU consumers their own libiconv-gnu.2.dylib.
needgnu=0
for d in "$FW"/*.dylib "$FW"/*.so; do [ -f "$d" ] && nm -u "$d" 2>/dev/null | grep -qw _libiconv && { needgnu=1; break; }; done
if [ "$needgnu" = 1 ] && ! nm -gU "$FW/libiconv.2.dylib" 2>/dev/null | grep -qw _libiconv; then
  log "splitting Apple/GNU libiconv (dylibbundler name-collision)"
  # Ensure libiconv.2.dylib is the Apple one (has _iconv) for glib & co.
  if ! nm -gU "$FW/libiconv.2.dylib" 2>/dev/null | grep -qw _iconv; then
    for p in /nix/store/*libiconv*/lib/libiconv.2.dylib; do
      nm -gU "$p" 2>/dev/null | grep -qw _iconv && { cp -f "$p" "$FW/libiconv.2.dylib"; install_name_tool -id @executable_path/../Frameworks/libiconv.2.dylib "$FW/libiconv.2.dylib"; break; }
    done
  fi
  # Bundle a GNU libiconv (has _libiconv) under a distinct name + its libcharset.
  gnu=""; for p in /nix/store/*libiconv*/lib/libiconv.2.dylib; do nm -gU "$p" 2>/dev/null | grep -qw _libiconv && { gnu="$p"; break; }; done
  if [ -n "$gnu" ]; then
    cp -f "$gnu" "$FW/libiconv-gnu.2.dylib"; install_name_tool -id @executable_path/../Frameworks/libiconv-gnu.2.dylib "$FW/libiconv-gnu.2.dylib"
    gc=$(otool -L "$gnu" | awk '/libcharset/{print $1}' | grep '^/nix' | head -1 || true)
    if [ -n "$gc" ]; then
      cp -f "$(dirname "$gnu")/libcharset.1.dylib" "$FW/libcharset-gnu.1.dylib"
      install_name_tool -id @executable_path/../Frameworks/libcharset-gnu.1.dylib "$FW/libcharset-gnu.1.dylib"
      install_name_tool -change "$gc" @executable_path/../Frameworks/libcharset-gnu.1.dylib "$FW/libiconv-gnu.2.dylib"
    fi
    # Repoint every _libiconv consumer to the GNU copy.
    for d in "$FW"/*.dylib "$FW"/*.so; do
      [ -f "$d" ] || continue; [ "$(basename "$d")" = libiconv-gnu.2.dylib ] && continue
      if nm -u "$d" 2>/dev/null | grep -qw _libiconv; then
        ref=$(otool -L "$d" | awk '/Frameworks\/libiconv\.2\.dylib/{print $1}' | head -1)
        [ -n "$ref" ] && install_name_tool -change "$ref" @executable_path/../Frameworks/libiconv-gnu.2.dylib "$d"
      fi
    done
  fi
fi

# No packaged Mach-O may retain a build-machine Nix dependency. Such a bundle signs successfully but
# fails on an end-user machine at dyld startup.
while IFS= read -r -d '' binary; do
  if file "$binary" | grep -q 'Mach-O' && otool -L "$binary" | grep -q '/nix/store'; then
    otool -L "$binary" | grep '/nix/store' >&2
    die "unrelocated Nix dependency in $binary"
  fi
done < <(find "$MACOS" "$FW" "$RES" -type f -print0)

# 8. Strip + codesign, deepest first (any later edit invalidates a signature).
#    HL_SIGN_ID unset/"-" = ad-hoc (default). A "Developer ID Application: …" identity name turns on real
#    signing with hardened runtime + secure timestamp; HL_SIGN_KEYCHAIN[/_PW] selects the keychain holding it.
#    The JIT engines + daemon keep the allow-jit entitlement ($ENT) so they run under the hardened runtime.
SIGN_ID="${HL_SIGN_ID:--}"
SIGN_FLAGS=""
if [ "$SIGN_ID" != "-" ]; then
  SIGN_FLAGS="--options runtime --timestamp"
  if [ -n "${HL_SIGN_KEYCHAIN:-}" ]; then
    security unlock-keychain ${HL_SIGN_KEYCHAIN_PW:+-p "$HL_SIGN_KEYCHAIN_PW"} "$HL_SIGN_KEYCHAIN" 2>/dev/null || true
    SIGN_FLAGS="$SIGN_FLAGS --keychain $HL_SIGN_KEYCHAIN"
  fi
  log "stripping + signing (Developer ID: $SIGN_ID)"
else
  log "stripping + signing (ad-hoc)"
fi
chmod -R u+w "$APP"   # data copied from the nix store is read-only; codesign needs write access
find "$FW" "$RES/lib" -type f \( -name '*.dylib' -o -name '*.so' \) -print0 2>/dev/null | while IFS= read -r -d '' f; do
  /usr/bin/strip -x "$f" 2>/dev/null || true
  codesign -s "$SIGN_ID" $SIGN_FLAGS -f "$f" >/dev/null 2>&1 || true
done
for b in hl-daemon; do
  [ -f "$RES/$b" ] && codesign -s "$SIGN_ID" $SIGN_FLAGS -f --entitlements "$ENT" "$RES/$b" >/dev/null 2>&1 || true
done
# The compositor has no JIT entitlement and is signed like the CLI.
for b in hl-compositor; do [ -f "$RES/$b" ] && codesign -s "$SIGN_ID" $SIGN_FLAGS -f "$RES/$b" >/dev/null 2>&1 || true; done
codesign -s "$SIGN_ID" $SIGN_FLAGS -f "$MACOS/husklet" >/dev/null 2>&1 || true
codesign -s "$SIGN_ID" $SIGN_FLAGS -f "$APP" >/dev/null 2>&1 || true   # outermost signed last
codesign --verify --deep --strict "$APP" || die "bundle signature verification failed"
if [ "$SIGN_ID" != "-" ]; then
  codesign --verify --strict --verbose=2 "$APP" || die "Developer ID signature verification failed"
  log "signed + verified ($(codesign -dv "$APP" 2>&1 | awk -F= '/^Authority/{print $2; exit}'))"
fi

SIZE="$(du -sh "$APP" | cut -f1)"
chmod -R u+w "$FINAL_APP" 2>/dev/null || true
rm -rf "$FINAL_APP"
mv "$APP" "$FINAL_APP"
trap - EXIT
log "done -> $FINAL_APP ($SIZE)"
