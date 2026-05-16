#!/usr/bin/env bash
# Build Gyors.app: Rust staticlib + Swift executable + minimal bundle.
#
# Env flags:
#   WITH_SPARKLE=1   embed Sparkle.framework + define SPARKLE for the
#                    Swift compile. Off by default; the launcher
#                    builds + runs fine without it (Updater.swift
#                    falls back to a no-op stub and the "Check for
#                    Updates..." menu item stays hidden). When on,
#                    macos/Frameworks/Sparkle.framework must already
#                    exist - the script doesn't download it.
#   WITH_CLOUD=1     include cloud sync (gyors-cloud). On by default.
#                    WITH_CLOUD=0 builds a launcher with no sync
#                    surface: no FFI symbols, no SyncPanel, no
#                    background tick, no "Cloud Sync..." menu item.
#                    Skips the CLOUD / GYORS_CLOUD defines for
#                    Swift + the bridging header, and drops the
#                    `cloud` feature from the cargo build.
#   WITH_AI=1        include AI features (built-in `ai`/`ask`
#                    provider, AI transforms, Apple FM router,
#                    OpenAI/Anthropic/Ollama HTTP client). On by
#                    default. WITH_AI=0 strips the AI providers
#                    from gyors-providers, the AI Swift files, and
#                    the "Ask AI" command-palette row.
#
# WITH_CLOUD and WITH_AI compose independently. The script always
# passes --no-default-features to cargo and rebuilds the feature
# list from the two flags so one doesn't accidentally turn off the
# other.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

MODE="${1:-release}"
case "$MODE" in
    release)
        CARGO_FLAGS="--release"
        RUST_LIB_DIR="target/release"
        SWIFT_FLAGS=(-O)
        ;;
    debug)
        CARGO_FLAGS=""
        RUST_LIB_DIR="target/debug"
        SWIFT_FLAGS=(-Onone -g)
        ;;
    *)
        echo "usage: $0 [release|debug]" >&2
        exit 1
        ;;
esac

WITH_SPARKLE="${WITH_SPARKLE:-0}"
SPARKLE_SWIFT_FLAGS=()
SPARKLE_LINK_FLAGS=()
SPARKLE_FRAMEWORK_PATH="macos/Frameworks/Sparkle.framework"

WITH_CLOUD="${WITH_CLOUD:-1}"
WITH_AI="${WITH_AI:-1}"

# Build the cargo feature set explicitly so the two flags compose.
# We always pass --no-default-features and re-list the desired
# features rather than relying on cargo's default-on behaviour;
# otherwise WITH_CLOUD=0 would also strip AI (and vice versa).
CARGO_FEATURES=()
[[ "$WITH_CLOUD" == "1" ]] && CARGO_FEATURES+=("cloud")
[[ "$WITH_AI" == "1" ]] && CARGO_FEATURES+=("ai")

CARGO_FEATURE_FLAGS=(--no-default-features)
if (( ${#CARGO_FEATURES[@]} > 0 )); then
    # Bash 3 `IFS=,` join trick - portable on macOS.
    feature_csv=$(IFS=,; echo "${CARGO_FEATURES[*]}")
    CARGO_FEATURE_FLAGS+=(--features "$feature_csv")
fi

CLOUD_SWIFT_FLAGS=()
if [[ "$WITH_CLOUD" == "1" ]]; then
    # Swift conditional + bridging-header preprocessor flag both
    # need to be defined for the call sites and the C decls to
    # line up.
    CLOUD_SWIFT_FLAGS=(
        -D CLOUD
        -Xcc -DGYORS_CLOUD
    )
fi

AI_SWIFT_FLAGS=()
if [[ "$WITH_AI" == "1" ]]; then
    AI_SWIFT_FLAGS=(-D AI)
fi

if [[ "$WITH_SPARKLE" == "1" ]]; then
    if [[ ! -d "$SPARKLE_FRAMEWORK_PATH" ]]; then
        echo "error: WITH_SPARKLE=1 but $SPARKLE_FRAMEWORK_PATH not found" >&2
        echo "  download Sparkle from https://github.com/sparkle-project/Sparkle/releases" >&2
        echo "  unpack the framework into macos/Frameworks/" >&2
        exit 1
    fi
    SPARKLE_SWIFT_FLAGS=(
        -D SPARKLE
        -F "macos/Frameworks"
        -framework Sparkle
    )
    # Tell dyld where to find Sparkle once the app runs: bundled
    # frameworks live at Gyors.app/Contents/Frameworks/.
    SPARKLE_LINK_FLAGS=(
        -Xlinker -rpath -Xlinker "@executable_path/../Frameworks"
    )
fi

LIB_NAME="libgyors_ipc.a"
APP_NAME="Gyors"
BUILD_DIR="macos/build"
APP_BUNDLE="$BUILD_DIR/$APP_NAME.app"

echo "[1/4] cargo build $CARGO_FLAGS -p gyors-ipc ${CARGO_FEATURE_FLAGS[*]}"
export MACOSX_DEPLOYMENT_TARGET=14.0
cargo build $CARGO_FLAGS -p gyors-ipc "${CARGO_FEATURE_FLAGS[@]}"

LIB_PATH="$RUST_LIB_DIR/$LIB_NAME"
[[ -f "$LIB_PATH" ]] || { echo "error: $LIB_PATH not found" >&2; exit 1; }

echo "[2/4] Preparing bundle at $APP_BUNDLE"
rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS" "$APP_BUNDLE/Contents/Resources"

echo "[3/4] swiftc → $APP_BUNDLE/Contents/MacOS/$APP_NAME"
SWIFT_SOURCES=(macos/Sources/Gyors/*.swift)
# `${ARRAY[@]+"${ARRAY[@]}"}` is the set -u safe way to expand a
# possibly-empty bash array. macOS's bash 3.2 trips over the plain
# `"${ARRAY[@]}"` form when ARRAY is empty.
xcrun swiftc \
    "${SWIFT_FLAGS[@]}" \
    -swift-version 5 \
    -target arm64-apple-macos14 \
    -import-objc-header macos/Sources/Gyors/BridgingHeader.h \
    -framework AppKit -framework Carbon -framework Foundation -framework SwiftUI -framework Combine \
    ${SPARKLE_SWIFT_FLAGS[@]+"${SPARKLE_SWIFT_FLAGS[@]}"} \
    ${CLOUD_SWIFT_FLAGS[@]+"${CLOUD_SWIFT_FLAGS[@]}"} \
    ${AI_SWIFT_FLAGS[@]+"${AI_SWIFT_FLAGS[@]}"} \
    -L "$RUST_LIB_DIR" -lgyors_ipc \
    -Xlinker -sectcreate -Xlinker __TEXT -Xlinker __info_plist -Xlinker macos/Info.plist \
    ${SPARKLE_LINK_FLAGS[@]+"${SPARKLE_LINK_FLAGS[@]}"} \
    -o "$APP_BUNDLE/Contents/MacOS/$APP_NAME" \
    "${SWIFT_SOURCES[@]}"

echo "[4/5] Installing Info.plist + icon + menu-bar art"
cp macos/Info.plist "$APP_BUNDLE/Contents/Info.plist"
if [[ -d macos/Resources ]]; then
    cp -R macos/Resources/. "$APP_BUNDLE/Contents/Resources/"
fi

# WITH_SPARKLE: copy + re-sign the framework before the outer
# codesign pass. Order matters - codesign walks the bundle bottom-up
# and the outer signature has to seal a framework that's itself
# already signed, otherwise notarisation rejects the chain.
if [[ "$WITH_SPARKLE" == "1" ]]; then
    echo "[4.5/5] Embedding Sparkle.framework"
    mkdir -p "$APP_BUNDLE/Contents/Frameworks"
    cp -R "$SPARKLE_FRAMEWORK_PATH" "$APP_BUNDLE/Contents/Frameworks/"
    codesign --force --options runtime --sign - \
        "$APP_BUNDLE/Contents/Frameworks/Sparkle.framework"
fi

echo "[5/5] Ad-hoc codesign (suppresses Gatekeeper prompts on local dev)"
codesign --force --deep --sign - "$APP_BUNDLE"

echo
echo "Built: $APP_BUNDLE"
if [[ "$WITH_CLOUD" == "1" ]]; then
    echo "Cloud sync: on"
else
    echo "Cloud sync: OFF (rerun with WITH_CLOUD=1 to enable)"
fi
if [[ "$WITH_AI" == "1" ]]; then
    echo "AI features: on"
else
    echo "AI features: OFF (rerun with WITH_AI=1 to enable)"
fi
if [[ "$WITH_SPARKLE" == "1" ]]; then
    echo "Sparkle: embedded (auto-update menu item exposed)"
else
    echo "Sparkle: off (rerun with WITH_SPARKLE=1 to enable auto-update)"
fi
echo "Launch: open \"$APP_BUNDLE\""
