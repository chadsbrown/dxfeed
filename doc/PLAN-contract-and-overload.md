# Plan: dxfeed contract and overload fixes

Written 2026-11-14, ahead of CQ WW DX CW (Nov 28–29, 2026). Last updated
2026-11-14, after items 1–3 and the skimmer per-band fix were merged to
`master`.

Eight items in the order they should be done. Items 1–3 are merged; items 4–8
are open and can wait for capture data. The raw-feed recorder (see "Recorder"
below) is not started and comes before any of the open items, because it is
the only work with a hard deadline.

Churn figures below come from a synthetic load test (150 skimmer reports/s,
6000 stations, 5% busted calls, skimmer gating on), not real contest data.

| # | Item | Status | Complexity | Consumer change required |
|---|---|---|---|---|
| 1 | Tolerance-based spot identity | Merged (PR #1) | Medium | None |
| 2 | Guaranteed New/Withdraw lifecycle | Merged (PR #1) | Small–medium | None; clogger should fix withdraw-by-call |
| 3 | Wire up metrics | Merged (PR #1) | Small | None |
| 4 | Overload policy and lag signal | Open | Medium–large | None; clogger batching recommended |
| 5 | Material-change updates | Open | Small | Only if enabled: clogger must upsert |
| 6 | Slim events | Open | Small | Only if opted in: dxfeed-cli needs summary fields |
| 7 | Snapshot and filter-change retraction | Open | Small–medium | None |
| 8 | Populate or remove dead fields | Open | Small | Compile break if fields are removed |

## 1. Tolerance-based spot identity

**Status: merged** (PR #1, 2026-11-14) as `AggregatorConfig::match_tolerance`
(`MatchToleranceConfig { cw_hz: 300, ssb_hz: 1000, dig_hz: 100 }`), changeable
at runtime with `DxFeed::set_match_tolerance()`. A value of 0 disables
matching and falls back to pure bucketing. In the synthetic test the edge
station went from 100 New / 99 Withdraw to 1 New. With item 2 also in place,
consumer event volume went from 11–84% of input to about 2.3%, regardless of
skimmer spread.

The 300 Hz CW default is a guess until real contest data exists; the capture
is how it gets tuned.

- **Why:** frequencies were floored into fixed 200 Hz buckets
  (`src/freq.rs`, `freq_bucket`). A station near a bucket edge flipped keys
  as skimmers disagreed by 100 Hz, and each flip emitted a Withdraw and a
  New. In the synthetic test 11–84% of input reached the consumer, where it
  should be a few percent; one edge station produced 100 New and 99 Withdraw
  from 300 reports. Each flip also reset the correlation counts that
  `filter.json` relies on. No bucket size fixes this, because some stations
  always sit on an edge.
- **What was done:** before building a key, the aggregator looks up existing
  spots for the same call, band and mode. If one's anchor frequency is within
  the tolerance, the observation attaches to it (an emitted spot wins,
  otherwise the nearest anchor). Otherwise a new key is created; if an
  emitted spot exists outside the tolerance, that is a real QSY.
- **Where:** `process_observation` in `src/aggregator/core.rs`, plus
  `SpotTable::key_index` and `find_match()` in
  `src/aggregator/spot_table.rs`. The index covers all keys, not only emitted
  ones, and is maintained on every removal path.
- **Config:** the bucket sizes remain only to decide where a new spot is
  filed.
- **Consumers:** no API change. `SpotKey.freq_bucket_hz` is an anchor that no
  longer tracks the current frequency, so consumers must read `freq_hz` for
  display; clogger already does.

## 2. Guaranteed lifecycle

**Status: merged** (PR #1, 2026-11-14). Withdraw is emitted only, and always,
for spots the consumer was sent; `DxSpotEvent::withdraw_reason` says why
(`Expired`, `Qsy`, `Evicted`). `aggregator_lifecycle_invariant` in
`tests/property_tests.rs` enforces the contract. Still open on the clogger
side: withdraw by key or band, not by call alone (see "clogger follow-ups").

- **Why:** a consumer should be able to keep a correct table by following
  events alone. Three paths broke that:
  - **Coalesced QSY:** inside the 200 ms window the old spot was removed with
    no Withdraw.
  - **Cap eviction:** `evict_oldest_spot` removed spots silently.
  - **Phantom withdraws:** TTL expiry emitted a Withdraw for spots that were
    gated or filtered and never emitted.
- **clogger impact of phantom withdraws:** clogger withdraws by call, so an
  expired, never-shown duplicate of a call removed that station's live spot
  from the bandmap.
- **What was done:** a Withdraw is emitted only if the spot was emitted, and
  on every removal path. A QSY always emits the Withdraw/New pair.
  `Aggregator::update_aggregator_config` now returns the withdraws caused by
  a lowered `max_spots`, and the feed task forwards them.
- **Withdraw reasons:** `WithdrawReason` is `#[non_exhaustive]`. A fourth
  reason, "filtered", is not implemented; it belongs to item 7.
- **Consumers:**
  - `withdraw_reason` is additive, so it only breaks code that constructs
    `DxSpotEvent` literals.
  - `DedupeConfig::qsy_coalesce_window` and
    `DxFeed::set_qsy_coalesce_window()` are removed, which is a compile break
    for anything that used them. clogger does not.

## 3. Wire up metrics

**Status: merged** (PR #1, 2026-11-14). `DxFeed::metrics()` returns a
`DxFeedMetrics` snapshot: received, gated (by tag), filtered (by reason),
deduped, emitted (by kind), withdrawn (by reason), table size, and per-source
spots, unparsed lines and reconnects. `dxfeed-cli --json --metrics-interval N`
prints a snapshot to stderr every N seconds. Not yet done: clogger logging a
snapshot (see "clogger follow-ups"), and the outbox depth and lag figures,
which belong with item 4.

- **Why:** there was no way to tell whether the library was doing its job,
  and the contest assessment depends on it.
- **Identity:** received = gated + filtered + deduped + emitted New + emitted
  Update. Item 4 must keep this true or restate it.
- **Consumers:** none. `run_cluster_source` and `run_supervised_source` take
  an extra `SourceMetricsHandle` argument, which affects only code that
  drives sources directly rather than through `DxFeedBuilder`.

## 4. Overload policy and lag signal

- **Why:** the library exists to protect the logger, but both channels are
  256 slots with awaiting sends (`src/feed.rs`, `run_aggregator_task`). A
  slow logger stalls the aggregator, then the sources, then the TCP
  connection, with no indication. Timestamps are taken at socket read, so the
  lateness is invisible.
- **Change:** the aggregator never awaits the consumer. Events go into a
  per-key outbox that coalesces while the channel is full. An undelivered New
  followed by a Withdraw cancels out, and a newer Update replaces an older
  one. The outbox is bounded by table size.
- **Event sources:** the outbox must take events from all three places that
  produce them: observations, ticks, and `update_aggregator_config`.
- **Lag signal:** outbox depth and oldest-pending age go into
  `DxFeedMetrics`, and a `Lagged` event (or the existing `DxEvent::Error`) is
  emitted when coalescing happens.
- **Metrics:** today events are counted when the aggregator produces them.
  Once the outbox coalesces, produced and delivered differ, so add delivered
  and coalesced counters and say which one `spots_emitted` means.
- **Where:** `run_aggregator_task` in `src/feed.rs`. Announcements become
  droppable under pressure.
- **Complexity:** medium to large. This is the most intricate item, because
  what the consumer receives must still satisfy the item 2 contract. Extend
  `aggregator_lifecycle_invariant` to run through the outbox with a paused
  consumer.
- **Consumers:** no required change. Consumers may see revision gaps, and can
  optionally handle `Lagged`.
- **clogger recommendation:** drain the feed in batches and send one batched
  `AppEvent`, so the reducer snaps cursors and re-renders once per batch, not
  once per spot. Today every spot event rescans the bandmap per radio and
  shares the 256-slot `AppEvent` channel with everything else.

## 5. Material-change updates

- **Why:** with updates off (the default), consumers never see a refreshed
  frequency or last-seen time; with them on, every observation is an event.
  Since item 1, a spot absorbs reports within the match tolerance, so with
  updates off the consumer keeps the first reported frequency even when later
  reports agree on one up to 300 Hz away.
- **Change:** replace the `emit_updates` flag with a policy: never, material
  only, or all. Material means the displayed frequency moved past a
  threshold, the comment or quality tag changed, or a heartbeat interval
  elapsed.
- **Complexity:** small; it is a change to `decide_emission`.
- **Consumers:** the default stays off, so nothing changes until enabled.
  clogger must upsert by key before enabling it, because it currently pushes
  every Update as a new bandmap entry.

## 6. Slim events

- **Why:** every event clones up to 20 observations with two strings each;
  clogger reads three fields (call, frequency, mode).
- **Change:** a builder option for event detail, summary or full. Summary
  leaves `observations` empty and adds last spotter and SNR as flat fields.
- **Complexity:** small.
- **Consumers:** the default stays full. clogger can opt in with no other
  change; dxfeed-cli reads `observations.last()` and would need the summary
  fields.

## 7. Snapshot and filter-change retraction

- **Why:** a consumer that restarts its view cannot resync, and a filter
  reload leaves already-emitted spots that now fail the filter.
- **Change:** add `DxFeed::snapshot()` returning the currently emitted spots,
  via a command channel to the aggregator task. On filter update, re-evaluate
  emitted spots and withdraw failures with a new `WithdrawReason::Filtered`.
- **Already in place from item 2:** `WithdrawReason` is `#[non_exhaustive]`,
  so the new variant is additive; `SpotTable::iter()` exists for the
  re-evaluation pass; and config updates already return events that the feed
  task forwards.
- **Complexity:** small to medium. What remains is building a `SpotView` from
  stored state, which means keeping the last spotter's details on the spot.
- **Consumers:** none required. It makes changing filters mid-contest safe
  for clogger.

## 8. Dead fields

- **Why:** `quality_detail`, `confidence`, `spot_time` and `DxEvent::Error`
  are in the public model but never populated.
- **Change:**
  - **`quality_detail`:** populate it by calling the existing
    `compute_tag_with_detail`; it is useful for the assessment.
  - **`spot_time`:** populate it from the parsed line.
  - **`DxEvent::Error`:** use it for item 4's lag signal.
  - **`confidence`:** remove it unless a definition is decided.
- **Complexity:** small.
- **Consumers:** removing `confidence` is a compile break for anything that
  reads it; clogger does not.

## Fixed outside the numbered items

- **Skimmer verification is tracked per band** (PR #3, 2026-11-14). The
  skimmer engine kept one verified frequency per callsign, so a station
  reported on two bands at once was tagged `Qsy` on the second band and gated
  (`allow_qsy` defaults to false); the second band never reached the
  consumer. `verified_calls` in `src/skimmer/quality.rs` is now keyed by
  (call, band). A band change is `Valid` on the new band; `Qsy` means a move
  within one band.

## Recorder

**Status: not started. Deadline: Nov 28.**

- **Why:** the contest is the only source of real load data, and the
  tolerance, filter and overload decisions above all depend on it. A raw
  capture can be replayed through any later version of the aggregator; a log
  of post-filter events cannot.
- **Change:** a tool that writes every raw line with its receive timestamp
  from each node (RBN plus one human cluster), reconnecting on its own, and a
  replay tool that feeds a capture through the aggregator and reports
  metrics.
- **Fallback with no new code:**
  `dxfeed-cli --json --metrics-interval 60 --callsign <call> connect <host> <port> > events.jsonl`.
  This covers one source and records post-filter events only.
- **Before leaving it unattended:** test login against the exact nodes to be
  used (see "Smaller findings").

## clogger follow-ups

clogger has not been changed. None of these block dxfeed work.

- **Bump the pin:** clogger's `Cargo.lock` still pins dxfeed `389d6f0`, from
  before items 1–3. Run `cargo update -p dxfeed`. It has not been compiled
  against the new dxfeed; clogger uses none of the removed APIs, so it is
  expected to build.
- **Withdraw by key or band:** `AppEvent::SpotWithdrawn { call }` removes
  every bandmap entry for the call, so a withdraw on one band removes the
  station on all bands. Carry the band (or key) through
  `logger-runtime/src/dxfeed_adapter.rs` and match on it in
  `logger-core/src/reducer.rs`.
- **Upsert, not push:** New and Update both push onto the bandmap `Vec` with
  no dedupe. Required before enabling item 5.
- **Log metrics:** log a `feed.metrics()` snapshot every minute from
  `dxfeed_adapter.rs`.
- **Batch spot events:** see the recommendation under item 4.

## Smaller findings not covered above

- **The age filter is a no-op live.** `now` and `last_seen` are the same
  timestamp when the filter runs in `process_observation`, so `max_age_secs`
  never drops anything.
- **Filtering happens after ingest.** Spots on denied bands or modes still
  occupy the table; a cheap band/mode pre-check before ingest would avoid it.
- **Login waits for a newline-terminated prompt** (`src/source/cluster.rs`,
  `login`). Every test sends `login:` followed by a newline; a node that
  sends the prompt without one may loop on the 30 s login timeout. Not tested
  against live nodes.
- **Skimmer engine cap eviction picks the wrong victim.** When the engine
  hits its cap it drops the whole history of the call with the oldest first
  observation, which is likely the busiest station.

## Test gaps

- **Login:** no test uses a realistic prompt or a transcript from a real
  node.
- **Golden replay:** no test replays a real capture; this depends on the
  recorder.
- **Slow consumer:** no test pauses the consumer; this belongs with item 4.
- **High-rate counts:** `tests/high_rate.rs` passes if more than 100 of 2000
  spots arrive; it should assert exact counts.
- **Filter and aggregator together:** the age filter, correlation thresholds
  and hot reload are not tested through the aggregator.
- **Skimmer engine cap eviction:** untested.
- **README examples:** not compiled; a `cfg(doctest)` include would catch
  stale samples.
- **CI:** there is no workflow; tests run only locally.
- **Sleeps:** the network tests use real sleeps of up to 15 s.

## Open decisions

- **CW match tolerance:** 300 Hz is the default until the capture says
  otherwise.
- **Mode in the skimmer verified key:** verification is keyed by (call,
  band). Adding mode would matter only for a station reported on CW and a
  digital mode on the same band at once.
