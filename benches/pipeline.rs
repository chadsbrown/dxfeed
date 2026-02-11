//! Benchmarks for the DxFeed hot-path components.

use criterion::{Criterion, black_box, criterion_group, criterion_main};

use chrono::Utc;

use dxfeed::aggregator::core::{Aggregator, AggregatorConfig, IncomingObservation};
use dxfeed::domain::{Band, DxMode, OriginatorKind};
use dxfeed::filter::config::FilterConfigSerde;
use dxfeed::filter::evaluate::evaluate;
use dxfeed::model::{SourceId, SpotView};
use dxfeed::parser::spot::{ParsedSpot, parse_line};

// ---------------------------------------------------------------------------
// parse_line benchmark
// ---------------------------------------------------------------------------

fn bench_parse_line(c: &mut Criterion) {
    let lines = [
        "DX de W3LPL:     14025.0  JA1ABC       CQ                         1830Z",
        "DX de W3LPL-2:   14025.0  JA1ABC       CW    18 dB  25 WPM  CQ     1830Z",
        "DX de VE3NEA:     7025.5  DL1ABC       CQ TEST                    1831Z",
    ];

    c.bench_function("parse_line/dxspider", |b| {
        b.iter(|| parse_line(black_box(lines[0])))
    });

    c.bench_function("parse_line/rbn", |b| {
        b.iter(|| parse_line(black_box(lines[1])))
    });
}

// ---------------------------------------------------------------------------
// evaluate benchmark
// ---------------------------------------------------------------------------

fn bench_evaluate(c: &mut Criterion) {
    let filter = FilterConfigSerde::default()
        .validate_and_compile()
        .unwrap();
    let now = Utc::now();

    c.bench_function("evaluate/default_filter", |b| {
        let view = SpotView::test_default(now, "JA1ABC", 14_025_000, Band::B20, DxMode::CW);
        b.iter(|| evaluate(black_box(&view), black_box(&filter)))
    });

    // With band-deny filter
    let mut deny_cfg = FilterConfigSerde::default();
    deny_cfg.rf.band_deny.insert(Band::B80);
    deny_cfg.rf.band_deny.insert(Band::B160);
    let deny_filter = deny_cfg.validate_and_compile().unwrap();

    c.bench_function("evaluate/band_deny_filter", |b| {
        let view = SpotView::test_default(now, "JA1ABC", 14_025_000, Band::B20, DxMode::CW);
        b.iter(|| evaluate(black_box(&view), black_box(&deny_filter)))
    });
}

// ---------------------------------------------------------------------------
// aggregator ingest benchmark
// ---------------------------------------------------------------------------

fn bench_aggregator_ingest(c: &mut Criterion) {
    let filter = FilterConfigSerde::default()
        .validate_and_compile()
        .unwrap();

    c.bench_function("aggregator/process_observation", |b| {
        let mut agg = Aggregator::new(filter.clone(), None, AggregatorConfig::default(), None, None);
        let mut i = 0u64;

        b.iter(|| {
            let obs = IncomingObservation {
                parsed: ParsedSpot {
                    spotter_call: format!("SKIM-{}", i % 100),
                    dx_call: format!("DX{}", i % 10000),
                    freq_hz: 14_025_000 + (i % 100) * 10,
                    comment: None,
                    utc_time: None,
                },
                source_id: SourceId("bench".into()),
                originator_kind: OriginatorKind::Human,
                received_at: Utc::now(),
                rbn_fields: None,
            };
            let _ = black_box(agg.process_observation(obs));
            i += 1;
        })
    });
}

criterion_group!(benches, bench_parse_line, bench_evaluate, bench_aggregator_ingest);
criterion_main!(benches);
