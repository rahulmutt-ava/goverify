#!/usr/bin/env bash
# Cache-store stats (phase-5a spec §6 rider 2): entry counts and byte
# sizes per layer. Report-only — feeds the generics-blowup measurement
# (parent spec §16) and the shakeout addendum.
#
# macOS-specific (uses BSD `stat -f %z`): this is a dev/report tool, not
# part of CI, and the repo's dev platform is darwin.
set -euo pipefail
ROOT="${1:?usage: cache_stats.sh <cache-root>}"
for layer in extract scc query; do
  dir="$ROOT/$layer"
  if [ -d "$dir" ]; then
    count=$(find "$dir" -type f ! -name '*.lock' | wc -l | tr -d ' ')
    bytes=$(find "$dir" -type f ! -name '*.lock' -exec stat -f %z {} + 2>/dev/null | awk '{s+=$1} END {print s+0}')
    echo "$layer: $count entries, $bytes bytes"
  else
    echo "$layer: (absent)"
  fi
done
