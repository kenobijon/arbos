#!/usr/bin/env bash
# Build arbos-ffi for Apple device + simulator, generate Swift bindings, pack one
# xcframework. Run on a Mac with Xcode and the iOS SDK.
#
# Usage (from repo root):
#   ./scripts/build-xcframework.sh
#   OUT_DIR=dist/arbos-ffi ./scripts/build-xcframework.sh
#
# Versioning: ship the xcframework under the same release tag as the Mac build
# (workspace package version). See docs/crdt-sync.md § "arbos-ffi / xcframework".
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

OUT_DIR="${OUT_DIR:-$ROOT/target/xcframework}"
FFI_CRATE=arbos-ffi
LIB_NAME=arbos_ffi
# Keep lockstep with workspace.dependencies.uniffi (=0.32.2).
UNIFFI_VERSION="$(
  python3 - <<'PY'
import pathlib, re
text = pathlib.Path("Cargo.toml").read_text()
m = re.search(r'^uniffi\s*=\s*"=?([^"]+)"', text, re.M)
print(m.group(1) if m else "0.32.2")
PY
)"

DEVICE_TARGET=aarch64-apple-ios
SIM_TARGET=aarch64-apple-ios-sim

echo "==> rustup targets (${DEVICE_TARGET}, ${SIM_TARGET})"
rustup target add "$DEVICE_TARGET" "$SIM_TARGET"

echo "==> uniffi-bindgen ${UNIFFI_VERSION}"
TOOLS_ROOT="$ROOT/target/uniffi-tools"
BINDGEN="$TOOLS_ROOT/bin/uniffi-bindgen"
if [[ ! -x "$BINDGEN" ]] || ! "$BINDGEN" --version 2>/dev/null | grep -q "$UNIFFI_VERSION"; then
  cargo install uniffi --version "$UNIFFI_VERSION" --features cli \
    --root "$TOOLS_ROOT" --locked --force
fi

echo "==> cargo build -p ${FFI_CRATE} (device + simulator)"
cargo build -p "$FFI_CRATE" --release --locked --target "$DEVICE_TARGET"
cargo build -p "$FFI_CRATE" --release --locked --target "$SIM_TARGET"

DEVICE_LIB="$ROOT/target/${DEVICE_TARGET}/release/lib${LIB_NAME}.a"
SIM_LIB="$ROOT/target/${SIM_TARGET}/release/lib${LIB_NAME}.a"
test -f "$DEVICE_LIB"
test -f "$SIM_LIB"

GEN_DIR="$OUT_DIR/generated"
rm -rf "$OUT_DIR"
mkdir -p "$GEN_DIR"

echo "==> uniffi-bindgen generate (Swift)"
# Prefer the simulator staticlib for metadata extraction on Apple Silicon Macs.
"$BINDGEN" generate --library "$SIM_LIB" --language swift --out-dir "$GEN_DIR"

SWIFT_OUT="$GEN_DIR/${LIB_NAME}.swift"
# UniFFI may emit <module>.swift or arbos_ffi.swift depending on namespace.
if [[ ! -f "$SWIFT_OUT" ]]; then
  SWIFT_OUT="$(find "$GEN_DIR" -maxdepth 1 -name '*.swift' | head -n1)"
fi
test -n "$SWIFT_OUT" && test -f "$SWIFT_OUT"

HEADERS_DIR="$OUT_DIR/headers"
mkdir -p "$HEADERS_DIR"
# Modulemap + umbrella header from bindgen sit next to the .swift file.
cp "$GEN_DIR"/*.h "$HEADERS_DIR/" 2>/dev/null || true
cp "$GEN_DIR"/*.modulemap "$HEADERS_DIR/" 2>/dev/null || true
if [[ ! -f "$HEADERS_DIR/module.modulemap" ]]; then
  # Some UniFFI versions name it ${LIB_NAME}.modulemap
  for mm in "$HEADERS_DIR"/*.modulemap; do
    [[ -f "$mm" ]] || continue
    cp "$mm" "$HEADERS_DIR/module.modulemap"
    break
  done
fi

XC_DIR="$OUT_DIR/ArbosFFI.xcframework"
echo "==> xcodebuild -create-xcframework"
rm -rf "$XC_DIR"
xcodebuild -create-xcframework \
  -library "$DEVICE_LIB" -headers "$HEADERS_DIR" \
  -library "$SIM_LIB" -headers "$HEADERS_DIR" \
  -output "$XC_DIR"

cp "$SWIFT_OUT" "$OUT_DIR/ArbosFFI.swift"

# Rough size note for docs / release notes.
DEVICE_SIZE=$(wc -c <"$DEVICE_LIB" | tr -d ' ')
echo "==> done"
echo "    xcframework: $XC_DIR"
echo "    swift:       $OUT_DIR/ArbosFFI.swift"
echo "    device .a:   ${DEVICE_SIZE} bytes (~$((DEVICE_SIZE / 1024 / 1024)) MiB before strip/link)"
