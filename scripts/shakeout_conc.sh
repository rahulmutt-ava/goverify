#!/usr/bin/env bash
# Phase-7 concurrency shakeout (spec §9): run goverify check over a
# pinned golang.org/x/sync checkout — errgroup / semaphore /
# singleflight are the densest real-world goroutine+channel code in the
# stdlib orbit. Manual/nightly only — network clone on first run.
set -euo pipefail
PIN="${GOVERIFY_SHAKEOUT_CONC_REF:-v0.10.0}"
DIR=".goverify/shakeout/sync"
if [ ! -d "$DIR/.git" ]; then
  git clone --quiet https://github.com/golang/sync "$DIR"
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
