# Phase-4 shakeout: etcd-io/bbolt @ v1.4.0

Status: PENDING — run `mise run shakeout`, triage every finding below,
then fill in the tables. Exit criteria (spec §7): all findings triaged,
FP rate recorded, every fixed FP lands a corpus case, dispatch-precision
observations recorded for phase-5 planning.

## Run parameters
- goverify commit: <sha>
- bbolt ref: v1.4.0
- timeouts: infer 100 ms / obligation 250 ms (defaults)

## Findings triage
| # | pos | tag | verdict (TP/FP/unclear) | note / corpus case |
|---|-----|-----|-------------------------|--------------------|

## Totals
- findings: N; TP: N; FP: N (rate: N%); unclear: N
- wall clock (cold / warm cache):

## Dispatch-precision + phase-5 observations
- (carried T10 watch item, §16 dynamic dispatch, timeout-bound FNs)
