# dxfeed

**DX cluster spot aggregation for contest loggers.**

![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)
![MSRV: 1.85](https://img.shields.io/badge/MSRV-1.85-orange.svg)

A Rust library that connects to multiple DX cluster and Reverse Beacon Network
(RBN) telnet sources, aggregates spots with deduplication and frequency
bucketing, applies configurable filters, classifies skimmer quality
(CT1BOH/AR-Cluster style), resolves DXCC entities, and emits a unified stream
of `DxEvent`s. All configuration is hot-reloadable at runtime. Built on tokio.

## Quick Start

```rust,no_run
use std::time::Duration;
use dxfeed::feed::DxFeedBuilder;
use dxfeed::model::{DxEvent, SourceId, SpotEventKind};
use dxfeed::source::supervisor::SourceConfig;
use dxfeed::source::telnet::TelnetSourceConfig;

#[tokio::main]
async fn main() {
    let source = SourceConfig::Telnet(TelnetSourceConfig::new(
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

## Features

| Feature    | Default | Description |
|------------|---------|-------------|
| `serde`    | yes     | Derive `Serialize`/`Deserialize` on all public types |
| `raw-lines`| no      | Store raw spot line text in `SpotObservation::raw` |
| `cty`      | no      | Built-in `CtyResolver` (parses cty.dat for DXCC lookups) |
| `cli`      | no      | `dxfeed-cli` binary (implies `cty` + `serde`) |

## Architecture

```
Sources (telnet/RBN)
    |
    v
Aggregator
  ├── SpotTable    (dedup, freq bucketing, TTL eviction)
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
| `source` | Telnet and RBN connectors with supervised reconnect |
| `resolver` | `EntityResolver` / `EnrichmentResolver` traits, `CtyResolver` |
| `model` | `DxEvent`, `DxSpot`, `SpotObservation`, `SpotKey` |
| `domain` | `Band`, `DxMode`, `Continent`, `OriginatorKind`, etc. |
| `parser` | DX spot line parser, RBN comment field parser |
| `freq` | Frequency-to-band/mode mapping, bucket computation |
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

See `FilterConfigSerde` rustdoc for the full set of filter sections: `rf`,
`dx`, `spotter`, `content`, `correlation`, `skimmer`, `geo`, `enrichment`.

### Aggregator

```rust,no_run
use std::time::Duration;
use dxfeed::aggregator::core::{AggregatorConfig, DedupeConfig, FreqBucketConfig};

let config = AggregatorConfig {
    spot_ttl: Duration::from_secs(900),
    max_spots: 50_000,
    max_observations_per_spot: 20,
    freq_bucket: FreqBucketConfig {
        cw_bucket_hz: 10,
        ssb_bucket_hz: 1000,
        dig_bucket_hz: 100,
    },
    dedupe: DedupeConfig { emit_updates: false },
    ..Default::default()
};
```

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
list, skimmer quality tag, confidence level) plus a monotonically increasing
`revision` number per spot key.

## CLI

A test harness binary is available behind the `cli` feature:

```sh
cargo run --features cli -- --callsign W1AW telnet dx.example.com 7300
cargo run --features cli -- --callsign W1AW rbn
cargo run --features cli -- --callsign W1AW --json --emit-updates rbn
cargo run --features cli -- --dump-default-filter > filter.json
```

## Requirements

- Rust edition 2024, MSRV **1.85**
- Async runtime: tokio

## License

MIT
