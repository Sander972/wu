#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
script/bundle-linux --channel dev "$@"
# Renamed here rather than in script/bundle-linux because .github/workflows/release.yml uploads target/release/wu-linux-<arch>.tar.gz.
archive="${CARGO_TARGET_DIR:-target}/release/wu-linux-$(uname -m)"
mv -f "$archive.tar.gz" "$archive-dev.tar.gz"
