#!/usr/bin/env bash
# Phase-4 shakeout (spec §7): run goverify check over a pinned bbolt
# checkout. Manual/nightly only — network clone on first run.
set -euo pipefail
PIN="${GOVERIFY_SHAKEOUT_REF:-v1.4.0}"
DIR=".goverify/shakeout/bbolt"
if [ ! -d "$DIR/.git" ]; then
  git clone --quiet https://github.com/etcd-io/bbolt "$DIR"
fi
git -C "$DIR" fetch --quiet --tags
git -C "$DIR" checkout --quiet "$PIN"
cargo build --release -p goverify-cli
BIN="$(pwd)/target/release/goverify"
export GOVERIFY_EXTRACTOR_DIR="$(pwd)/extractor"
cd "$DIR"
# Exit 1 (findings) is the expected outcome; only 2 (analyzer error) fails.
set +e
"$BIN" check ./... --cache-dir "$(pwd)/../cache"
code=$?
set -e
if [ "$code" -eq 2 ]; then
  echo "shakeout: analyzer error" >&2
  exit 2
fi
echo "shakeout: exit $code (0 clean / 1 findings)" >&2
