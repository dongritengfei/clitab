#!/bin/sh
# Ad-hoc sign the macOS bundle artifacts.
#
# macOS 14+ Gatekeeper rejects completely unsigned apps even when they never
# crossed the network: locally built copies carry com.apple.provenance, and
# launching one prompts "must be moved to the trash". An ad-hoc signature
# (codesign -s -) is enough for that local path — no Apple Developer account
# needed. Run this right after `tauri build`.
#
# Distributing the app (downloaded DMG => com.apple.quarantine on the other
# machine) still requires a Developer ID signature plus notarization; when a
# certificate exists, drop this script and let Tauri sign natively via
# APPLE_SIGNING_IDENTITY / APPLE_CERTIFICATE instead.
set -eu

signed=0
# Per-target bundle dirs as well as the host-arch default, so a multi-arch
# release build (tauri build --target <triple>) gets every artifact signed.
for bundle_dir in "src-tauri/target/release/bundle" src-tauri/target/*/release/bundle; do
    [ -d "$bundle_dir" ] || continue
    for artifact in "$bundle_dir"/macos/*.app "$bundle_dir"/dmg/*.dmg; do
        [ -e "$artifact" ] || continue
        codesign --force --sign - "$artifact"
        codesign --verify --verbose=2 "$artifact" 2>&1 | sed -n '1p'
        signed=$((signed + 1))
    done
done

if [ "$signed" -eq 0 ]; then
    echo "no bundle artifacts found under src-tauri/target (run tauri build first)" >&2
    exit 1
fi
echo "ad-hoc signed $signed artifact(s)"
