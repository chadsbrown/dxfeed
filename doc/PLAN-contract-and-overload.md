# Plan: dxfeed contract and overload fixes

Written 2026-11-14, ahead of CQ WW DX CW (Nov 28–29, 2026).

Eight items in the order they should be done. Items 1–3 are the ones worth
landing before the contest; the rest can wait for capture data. A raw-feed
recorder (separate work) should go first, so the contest is captured whatever
else lands.

Churn figures below come from a synthetic load test (150 skimmer reports/s,
6000 stations, 5% busted calls, skimmer gating on), not real contest data.

| # | Item | Complexity | Consumer change required |
|---|---|---|---|
| 1 | Tolerance-based spot identity | Medium | None |
| 2 | Guaranteed New/Withdraw lifecycle | Small–medium | None; clogger should fix withdraw-by-call |
| 3 | Wire up metrics | Small | None |
| 4 | Overload policy and lag signal | Medium–large | None; clogger batching recommended |
| 5 | Material-change updates | Small | Only if enabled: clogger must upsert |
| 6 | Slim events | Small | Only if opted in: dxfeed-cli needs summary fields |
| 7 | Snapshot and filter-change retraction | Medium | None |
| 8 | Populate or remove dead fields | Small | Compile break if fields are removed |

## 1. Tolerance-based spot identity

**Status: implemented** on branch `tolerance-spot-identity` (2026-11-14) as
`AggregatorConfig::match_tolerance`, default 300 Hz for CW, client
configurable. In the synthetic test the edge station went from 100 New / 99
Withdraw to 1 New, and consumer event volume from 11–84% of input to about
4.5% regardless of skimmer spread. Roughly half of what remains is the
phantom withdraws described in item 2. `qsy_coalesce_window` is still in
place; removing it belongs with item 2.

- **Why:** frequencies are floored into fixed 200 Hz buckets
  (`src/freq.rs`, `freq_bucket`). A station near a bucket edge flips keys as
  skimmers disagree by 100 Hz, and each flip emits a Withdraw and a New. In
  the synthetic test 11–84% of input reaches the consumer, where it should be
  a few percent; one edge station produced 100 New and 99 Withdraw from 300
  reports. Each flip also resets the correlation counts that `filter.json`
  relies on. No bucket size fixes this, because some stations always sit on
  an edge.
- **Change:** before building a key, look up existing spots for the same
  call, band and mode. If one's anchor frequency is within a tolerance (about
  300 Hz for CW, matching the skimmer engine's window), attach the
  observation to it. Otherwise create a new key; if an emitted spot exists
  outside the tolerance, that is a real QSY.
- **Where:** `process_observation` in `src/aggregator/core.rs`, plus a
  secondary index in `src/aggregator/spot_table.rs` covering all keys, not
  only emitted ones. The index must be maintained in all three removal paths
  (TTL expiry, cap eviction, explicit removal).
- **Config:** add a per-mode `match_tolerance_hz`. The bucket sizes remain
  only as the anchor for new keys, and `qsy_coalesce_window` becomes
  unnecessary and can be deprecated.
- **Complexity:** medium. The logic is contained, but about a dozen existing
  tests assert bucket behaviour and need rewriting.
- **Consumers:** no API change. `SpotKey.freq_bucket_hz` becomes an anchor
  that no longer tracks the current frequency, so consumers must read
  `freq_hz` for display; clogger already does.

## 2. Guaranteed lifecycle

**Status: implemented** on branch `tolerance-spot-identity` (2026-11-14).
Withdraw is emitted only, and always, for spots the consumer was sent;
`DxSpotEvent::withdraw_reason` says why (`Expired`, `Qsy`, `Evicted`).
`qsy_coalesce_window` and `DxFeed::set_qsy_coalesce_window()` are removed.
`aggregator_lifecycle_invariant` in `tests/property_tests.rs` enforces the
contract. Still open on the clogger side: withdraw by key or band, not by
call alone.

- **Why:** a consumer should be able to keep a correct table by following
  events alone. Today three paths break that:
  - **Coalesced QSY:** inside the 200 ms window the old spot is removed with
    no Withdraw (`core.rs`, the `coalesce` branch of `process_observation`).
  - **Cap eviction:** `evict_oldest_spot` removes spots silently
    (`spot_table.rs`).
  - **Phantom withdraws:** TTL expiry emits a Withdraw for spots that were
    gated or filtered and never emitted (`Aggregator::tick`).
- **clogger impact of phantom withdraws:** clogger withdraws by call, so an
  expired, never-shown duplicate of a call removes that station's live spot
  from the bandmap.
- **Change:** emit a Withdraw only if `first_emitted` is set, and emit one on
  every removal path. Always emit the Withdraw/New pair on QSY; item 1
  removes the jitter that the coalesce window was hiding. Add a `reason` to
  Withdraw (expired, QSY, evicted, filtered).
- **Test:** a property test that replays a random stream into a model
  consumer and asserts every New gets exactly one Withdraw and nothing
  arrives for a dead key.
- **Complexity:** small to medium.
- **Consumers:** the new field is additive, so it only breaks code that
  constructs `DxSpotEvent` literals. clogger should carry band (or the key)
  through `AppEvent::SpotWithdrawn` and match on it; that is a clogger change
  in `logger-runtime/src/dxfeed_adapter.rs` and `logger-core/src/reducer.rs`.

## 3. Wire up metrics

**Status: implemented** on branch `tolerance-spot-identity` (2026-11-14).
`DxFeed::metrics()` returns a `DxFeedMetrics` snapshot: received, gated (by
tag), filtered (by reason), deduped, emitted (by kind), withdrawn (by
reason), table size, and per-source spots, unparsed lines and reconnects.
`dxfeed-cli --json --metrics-interval N` prints a snapshot to stderr every N
seconds. Not yet done: clogger logging a snapshot, and the outbox depth and
lag figures, which belong with item 4.

- **Why:** there is currently no way to tell whether the library is doing its
  job, and the contest assessment depends on it. `MetricsHandle` in
  `src/metrics.rs` exists and is tested but is never referenced elsewhere.
- **Change:** thread `MetricsHandle` through sources and aggregator, and
  expose `DxFeed::metrics()`. Count received per source, gated per quality
  tag, dropped per `FilterDropReason`, emitted per kind, table size, unparsed
  lines and reconnects.
- **Complexity:** small.
- **Consumers:** none. clogger could log a snapshot every minute.

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
- **Lag signal:** outbox depth and oldest-pending age go into metrics, and a
  `Lagged` event (or the existing `DxEvent::Error`) is emitted when
  coalescing happens.
- **Where:** `run_aggregator_task` in `src/feed.rs`. Announcements become
  droppable under pressure.
- **Complexity:** medium to large. This is the most intricate item, because
  ordering must still satisfy the item 2 invariant; it needs tests with a
  paused consumer.
- **Consumers:** no required change. Consumers may see revision gaps, and can
  optionally handle `Lagged`.
- **clogger recommendation:** drain the feed in batches and send one batched
  `AppEvent`, so the reducer snaps cursors and re-renders once per batch, not
  once per spot. Today every spot event rescans the bandmap per radio and
  shares the 256-slot `AppEvent` channel with everything else.

## 5. Material-change updates

- **Why:** with updates off (the default), consumers never see a refreshed
  frequency or last-seen time; with them on, every observation is an event.
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
  emitted spots and withdraw failures with reason "filtered".
- **Complexity:** medium. Re-evaluation needs a `SpotView` built from stored
  state, which means keeping the last spotter's details on the spot.
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

## Smaller findings not covered above

- **The age filter is a no-op live.** `now` and `last_seen` are the same
  timestamp when the filter runs in `process_observation`, so `max_age_secs`
  never drops anything.
- **Filtering happens after ingest.** Spots on denied bands or modes still
  occupy the table; a cheap band/mode pre-check before ingest would avoid it.
- **Login waits for a newline-terminated prompt** (`src/source/cluster.rs`,
  `login`). Not tested against live nodes as part of this review; verify
  against each node before relying on it unattended.

## Open decisions

- **Item 1 before the contest, or after the capture?** Recommended: land 1–3
  before, so the live run measures the fixed behaviour, with the raw capture
  as the fallback for tuning the tolerance afterwards. The catch is that the
  tolerance value is a guess until real data exists.
