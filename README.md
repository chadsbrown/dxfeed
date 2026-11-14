# dxfeed

**DX cluster spot aggregation for contest loggers.**

A Rust library that connects to one or more DX cluster nodes, aggregates
spots with deduplication and tolerance-based frequency matching, applies configurable
filters, classifies skimmer quality (CT1BOH/AR-Cluster style), resolves
DXCC entities, and emits a unified stream of `DxEvent`s. All configuration
is hot-reloadable at runtime. Built on tokio.

## Quick Start

```rust,no_run
use std::time::Duration;
use dxfeed::feed::DxFeedBuilder;
use dxfeed::model::{DxEvent, SourceId, SpotEventKind};
use dxfeed::source::cluster::ClusterSourceConfig;
use dxfeed::source::supervisor::SourceConfig;

#[tokio::main]
async fn main() {
    let source = SourceConfig::Cluster(ClusterSourceConfig::new(
        "dx.example.com",
        7300,
        "W1AW",
        SourceId("cluster1".into()),
    ));

    let mut feed = DxFeedBuilder::new()
        .add_source(source)
        .build()
        .expect("failed to build feed");

    while let Some(event) = feed.next_event().await {
        match event {
            DxEvent::Spot(e) => {
                let s = &e.spot;
                println!(
                    "{} {} {:.1} kHz {} [{}]",
                    match e.kind {
                        SpotEventKind::New => "NEW",
                        SpotEventKind::Update => "UPD",
                        SpotEventKind::Withdraw => "DEL",
                    },
                    s.dx_call,
                    s.freq_hz as f64 / 1000.0,
                    s.mode,
                    s.band,
                );
            }
            DxEvent::SourceStatus(s) => eprintln!("source {:?}: {:?}", s.source_id, s.state),
            DxEvent::Announce(a) => eprintln!("announce: {}", a.text),
            DxEvent::Error(e) => eprintln!("error: {}", e.message),
        }
    }
}
```

### Originator Policy

`ClusterSourceConfig` includes an `originator_policy` field that controls
how spots are classified:

| Policy | Description |
|--------|-------------|
| `Auto` (default) | Infer per-spot from spotter callsign pattern (e.g., `-N` suffix = skimmer) |
| `AllSkimmer` | Force all spots from this connection to `OriginatorKind::Skimmer` |
| `AllHuman` | Force all spots from this connection to `OriginatorKind::Human` |

## Features

| Feature    | Default | Description |
|------------|---------|-------------|
| `serde`    | yes     | Derive `Serialize`/`Deserialize` on all public types |
| `raw-lines`| no      | Store raw spot line text in `SpotObservation::raw` |
| `cty`      | no      | Built-in `CtyResolver` (parses cty.dat for DXCC lookups) |
| `cli`      | no      | `dxfeed-cli` binary (implies `cty` + `serde`) |

## Architecture

```
Cluster Sources
    |
    v
Aggregator
  ├── SpotTable    (dedup, freq tolerance matching, TTL eviction)
  ├── SkimmerQuality (CT1BOH-style Valid/Busted/Qsy classification)
  └── Filter       (band/mode/callsign/geo/correlation/enrichment rules)
    |
    v
DxEvent stream  -->  your application
```

### Key Modules

| Module | Purpose |
|--------|---------|
| `feed` | `DxFeedBuilder` / `DxFeed` — top-level API, spawns sources and aggregator |
| `aggregator` | `Aggregator`, `SpotTable`, `AggregatorConfig` — core pipeline |
| `filter` | `FilterConfigSerde` — declarative filter rules (band, mode, callsign, geo, correlation, etc.) |
| `skimmer` | `SkimmerQualityEngine`, `SkimmerQualityConfig` — skimmer gating |
| `source` | Cluster source connector with supervised reconnect |
| `resolver` | `EntityResolver` / `EnrichmentResolver` traits, `CtyResolver` |
| `model` | `DxEvent`, `DxSpot`, `SpotObservation`, `SpotKey` |
| `domain` | `Band`, `DxMode`, `Continent`, `OriginatorKind`, etc. |
| `parser` | DX spot line parser, skimmer comment field parser |
| `freq` | Frequency-to-band/mode mapping, bucket computation |
| `metrics` | `DxFeedMetrics` — pipeline counters via `DxFeed::metrics()` |
| `callsign` | Normalization, portable suffix stripping, skimmer detection |

## Configuration

### Filtering

Filters are defined with `FilterConfigSerde` and compiled at build time (or
hot-reloaded). Example JSON:

```json
{
  "max_age_secs": 900,
  "freq_sanity_hz": [1800000, 54000000],
  "rf": {
    "band_deny": ["B160", "B6"],
    "mode_allow": ["CW", "SSB"]
  },
  "dx": {
    "callsign_deny": ["BEACON*"]
  },
  "correlation": {
    "min_unique_originators": 2
  }
}
```

For the complete configuration reference, see [`doc/CONFIGURATION.md`](doc/CONFIGURATION.md).

### Aggregator

```rust,no_run
use std::time::Duration;
use dxfeed::aggregator::core::{AggregatorConfig, DedupeConfig, MatchToleranceConfig};

let config = AggregatorConfig {
    spot_ttl: Duration::from_secs(900),
    max_spots: 50_000,
    max_observations_per_spot: 20,
    match_tolerance: MatchToleranceConfig {
        cw_hz: 300,
        ssb_hz: 1000,
        dig_hz: 100,
    },
    dedupe: DedupeConfig {
        emit_updates: false,
        ..Default::default()
    },
    ..Default::default()
};
```

Two reports are the same spot when they have the same callsign, band and
mode and the new frequency is within `match_tolerance` of the frequency at
which the spot was first seen. Outside the tolerance the station has moved:
the old spot is withdrawn and a new one emitted.

### Skimmer Quality

```rust,no_run
use dxfeed::skimmer::config::SkimmerQualityConfig;

let config = SkimmerQualityConfig {
    gate_skimmer_output: true,
    valid_required_distinct_skimmers: 3,
    allow_unknown: false,
    allow_busted: false,
    ..Default::default()
};
```

When gating is enabled, single-skimmer reports are held until corroborated by
the required number of distinct skimmers within the frequency/time window.
Human-originated spots always pass through.

## Hot Reload

All configuration can be updated at runtime without restarting connections:

```rust,no_run
# use dxfeed::feed::DxFeed;
# fn example(feed: &DxFeed) {
use std::time::Duration;
use dxfeed::aggregator::core::MatchToleranceConfig;
use dxfeed::filter::config::FilterConfigSerde;
use dxfeed::domain::Band;

// Replace the entire filter
let mut filter = FilterConfigSerde::default();
filter.rf.band_deny.insert(Band::B160);
feed.update_filter(filter).unwrap();

// Convenience setters for common changes
feed.set_spot_ttl(Duration::from_secs(600));
feed.set_emit_updates(true);
feed.set_max_spots(100_000);
feed.set_match_tolerance(MatchToleranceConfig { cw_hz: 200, ..Default::default() });

// Skimmer quality tuning
feed.set_skimmer_gate(true);
feed.set_allow_unverified(false);
feed.set_required_distinct_skimmers(2);
# }
```

## Resolvers

### Entity Resolution

Implement the `EntityResolver` trait to provide DXCC lookups:

```rust,no_run
use dxfeed::resolver::entity::{EntityResolver, EntityInfo};

struct MyResolver { /* ... */ }

impl EntityResolver for MyResolver {
    fn resolve(&self, callsign: &str) -> Option<EntityInfo> {
        // Look up callsign in cty.dat, database, etc.
        todo!()
    }
}
```

Enable the `cty` feature for the built-in `CtyResolver` that parses cty.dat files.

### Enrichment Resolution

Implement `EnrichmentResolver` to provide LoTW, master DB, callbook, and
club membership data:

```rust,no_run
use std::collections::BTreeSet;
use dxfeed::resolver::enrichment::EnrichmentResolver;

struct MyEnrichment { /* ... */ }

impl EnrichmentResolver for MyEnrichment {
    fn lotw_user(&self, callsign: &str) -> Option<bool> { todo!() }
    fn in_master_db(&self, callsign: &str) -> Option<bool> { todo!() }
    fn in_callbook(&self, callsign: &str) -> Option<bool> { todo!() }
    fn memberships(&self, callsign: &str) -> Option<BTreeSet<String>> { todo!() }
}
```

## Events

`DxEvent` is the top-level output type:

| Variant | Description |
|---------|-------------|
| `Spot(DxSpotEvent)` | A spot lifecycle event (`New`, `Update`, or `Withdraw`) |
| `Announce(DxAnnounce)` | Text announcement from a cluster node |
| `SourceStatus(SourceStatus)` | Connection state change (`Connecting`, `Connected`, `Reconnecting`, `Failed`, `Shutdown`) |
| `Error(DxError)` | Non-fatal error from a source or the aggregator |

Each `DxSpotEvent` carries the full `DxSpot` (aggregated data, observations
list, skimmer quality tag) plus a monotonically increasing `revision` number
per spot key.

The lifecycle is reliable: per spot key, the first event is `New`, every
`New` is eventually followed by exactly one `Withdraw`, and nothing follows
a `Withdraw` except a fresh `New`. A consumer can keep a correct spot table
from events alone. Each `Withdraw` carries a `withdraw_reason`:

| Reason | Meaning |
|--------|---------|
| `Expired` | No observation arrived within `spot_ttl`. |
| `Qsy` | The station moved; a `New` for the new frequency follows immediately. |
| `Evicted` | Removed to keep the table within `max_spots`. |

## Metrics

`DxFeed::metrics()` returns a `DxFeedMetrics` snapshot of counters kept
since the feed was built. It is cheap and can be polled freely.

```rust,ignore
let m = feed.metrics();
println!(
    "received {} gated {} filtered {} deduped {} -> new {} update {} withdraw {}",
    m.spots_received, m.spots_gated, m.spots_filtered, m.spots_deduped,
    m.emitted_new, m.emitted_update, m.emitted_withdraw,
);
```

Every observation ends in exactly one outcome, so
`spots_received = spots_gated + spots_filtered + spots_deduped + emitted_new + emitted_update`.

| Field | Meaning |
|-------|---------|
| `spots_received` | Observations that reached the aggregator. |
| `spots_gated`, `skimmer_gated` | Blocked by skimmer quality gating, total and by tag. |
| `skimmer_tagged` | All skimmer observations by quality tag. |
| `spots_filtered`, `filtered_by_reason` | Dropped by the filter, total and by `FilterDropReason`. |
| `spots_deduped` | Passed, but the spot was already emitted. |
| `emitted_new`, `emitted_update`, `emitted_withdraw` | Events produced for the consumer. |
| `withdrawn` | Withdraws by reason (`expired`, `qsy`, `evicted`). |
| `spot_table_size` | Current spots in the table (gauge). |
| `sources` | Per source: `spots_received`, `unparsed_lines`, `reconnects`. |
| `parse_errors`, `reconnects` | Totals across sources. |

`unparsed_lines` counts lines after login that were not a spot,
announcement, propagation report or prompt. A steadily climbing count
against one node suggests a spot format the parser does not recognise.

## CLI

A test harness binary is available behind the `cli` feature:

```sh
cargo run --features cli -- --callsign W1AW connect dx.example.com 7300
cargo run --features cli -- --callsign W1AW connect dx.example.com 7300 --originator-policy all-skimmer
cargo run --features cli -- --callsign W1AW --json --metrics-interval 60 connect dx.example.com 7300
cargo run --features cli -- --dump-default-filter > filter.json
```

With `--json`, spot events go to stdout as one JSON object per line, and
`--metrics-interval N` prints a metrics snapshot to stderr every N seconds.

## Requirements

- Rust edition 2024, MSRV **1.85**
- Async runtime: tokio

## License

MIT
