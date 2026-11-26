#!/bin/sh
# Build xrmimic even while a Chrome holds target/release/xrmimic_runtime.dll (or the
# launcher helper holds xrmimic-launch.exe): Windows lets a loaded file be
# renamed, so a locked one is moved aside first.
set -e
cd "$(dirname "$0")/.."
# Cargo also keeps hard-linked copies under deps/, locked along with them.
for f in target/release/xrmimic_runtime.dll target/release/xrmimic-launch.exe \
         target/release/deps/xrmimic_runtime.dll target/release/deps/xrmimic_launch.exe; do
    if [ -f "$f" ] && ! (: >> "$f") 2>/dev/null; then
        mv "$f" "$f.locked-$(date +%s)"
    fi
done
rm -f target/release/*.locked-* target/release/deps/*.locked-* 2>/dev/null || true
cargo build --release -p xrmimic-runtime -p xrmimic-launch "$@"
