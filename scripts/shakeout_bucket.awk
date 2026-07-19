#!/usr/bin/awk -f
# Parse `goverify check` rendered output (frozen format,
# crates/goverify-cli/src/render.rs) into one TSV row per finding and
# assign a coarse triage class (design spec 2026-07-19, section 2.2):
#   pos  tag  func  message  source_line  has_trace  model  class
# class = tag "|" normalized source line (identifiers -> I, numbers -> N,
# string literals -> S, whitespace collapsed), or tag "|msg:" normalized
# message when the block has no source echo.
# Loud-fail contract: any unrecognized line shape, a block without a
# header, or zero findings aborts with exit 1 (stderr says why). stdout
# is data only. Portable: BSD awk, no gawk extensions.

function fail(why) {
  printf "shakeout_bucket: line %d: %s\n", NR, why > "/dev/stderr"
  bad = 1
  exit 1
}

function normalize(s,   t) {
  t = s
  gsub(/"[^"]*"/, "S", t)
  gsub(/`[^`]*`/, "S", t)
  gsub(/[0-9][0-9]*/, "N", t)
  gsub(/[A-Za-z_][A-Za-z0-9_]*/, "I", t)
  gsub(/[ \t][ \t]*/, " ", t)
  sub(/^ /, "", t)
  sub(/ $/, "", t)
  return t
}

function flush_block(   key) {
  if (!have) return
  key = tag "|" (src != "" ? normalize(src) : "msg:" normalize(msg))
  printf "%s\t%s\t%s\t%s\t%s\t%d\t%s\t%s\n", \
    pos, tag, fn, msg, src, has_trace, model, key
  emitted++
  have = 0; pos = ""; tag = ""; fn = ""; msg = ""; src = ""
  model = ""; has_trace = 0
}

/^$/ { flush_block(); next }

# Header: <file>:<line>:<col>: <tag>: <message> [<func>]. The func id is
# everything after the LAST " [" (ssa ids contain no " [" sequence; a
# violation of that assumption trips the tag plausibility check below).
/^[^ ].*\]$/ && !have {
  line = $0
  sub(/\]$/, "", line)
  j = 0
  while (m = index(substr(line, j + 1), " [")) j += m
  if (j == 0) fail("header without [func]: " $0)
  fn = substr(line, j + 2)
  line = substr(line, 1, j - 1)
  if (line ~ /^-:-:-: /) {
    pos = "-:-:-"
    rest = substr(line, 8)
  } else if (match(line, /^[^:]+:[0-9]+:[0-9]+: /)) {
    pos = substr(line, 1, RLENGTH - 2)
    rest = substr(line, RLENGTH + 1)
  } else fail("unparseable position in header: " $0)
  k = index(rest, ": ")
  if (k == 0) fail("header without tag separator: " $0)
  tag = substr(rest, 1, k - 1)
  msg = substr(rest, k + 2)
  if (tag !~ /^[a-z][a-z-]*$/) fail("implausible tag '" tag "': " $0)
  have = 1
  headers++
  next
}

/^ *[0-9][0-9]* \| / && have {
  src = $0
  sub(/^ *[0-9][0-9]* \| /, "", src)
  next
}

/^ +\| *\^$/ && have { next }

/^    path: / && have { has_trace = 1; next }

/^    with: / && have { model = substr($0, 11); next }

{ fail("unrecognized line shape: " $0) }

END {
  if (bad) exit 1
  flush_block()
  if (headers == 0) fail("no findings parsed")
  if (emitted != headers) {
    printf "shakeout_bucket: emitted %d rows for %d headers\n", \
      emitted, headers > "/dev/stderr"
    exit 1
  }
  printf "shakeout_bucket: %d findings\n", emitted > "/dev/stderr"
}
