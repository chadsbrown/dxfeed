//! dxfeed-cli — test harness for the dxfeed library.
//!
//! Connects to a DX cluster node and prints aggregated spots to stdout.

use std::path::PathBuf;
use std::process;
use std::time::Duration;

use clap::{Parser, Subcommand};

use dxfeed::aggregator::core::{AggregatorConfig, DedupeConfig};
use dxfeed::feed::DxFeedBuilder;
use dxfeed::filter::config::FilterConfigSerde;
use dxfeed::model::{
    DxEvent, DxSpotEvent, SkimQualityTag, SourceId, SpotEventKind,
};
use dxfeed::skimmer::config::SkimmerQualityConfig;
use dxfeed::source::cluster::{ClusterSourceConfig, OriginatorPolicy};
use dxfeed::source::supervisor::SourceConfig;

// ---------------------------------------------------------------------------
// CLI argument definitions
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "dxfeed-cli", about = "DX cluster spot aggregator test tool")]
struct Cli {
    /// Callsign for cluster login.
    #[arg(short, long, global = true)]
    callsign: Option<String>,

    /// Source label.
    #[arg(long, default_value = "src0", global = true)]
    source_id: String,

    /// Cluster login password.
    #[arg(long, global = true)]
    password: Option<String>,

    /// Path to a JSON filter configuration file.
    #[arg(long, global = true)]
    filter_file: Option<PathBuf>,

    /// Path to a cty.dat file for DXCC resolution.
    #[arg(long, global = true)]
    cty_file: Option<PathBuf>,

    /// Emit Update events (not just New/Withdraw).
    #[arg(long, global = true)]
    emit_updates: bool,

    /// Disable skimmer quality gating.
    #[arg(long, global = true)]
    no_skimmer_quality: bool,

    /// Spot TTL in seconds.
    #[arg(long, default_value = "900", global = true)]
    spot_ttl: u64,

    /// Output events as JSON (one object per line).
    #[arg(long, global = true)]
    json: bool,

    /// Print status/announce events to stderr.
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Print the default FilterConfigSerde as JSON and exit.
    #[arg(long)]
    dump_default_filter: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Connect to a DX cluster node.
    Connect {
        /// Cluster hostname or IP.
        host: String,
        /// Cluster port.
        port: u16,
        /// Originator detection policy: auto, all-skimmer, all-human.
        #[arg(long, default_value = "auto")]
        originator_policy: String,
    },
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // --dump-default-filter: print and exit
    if cli.dump_default_filter {
        let cfg = FilterConfigSerde::default();
        let json = serde_json::to_string_pretty(&cfg).expect("serialize default filter");
        println!("{json}");
        return;
    }

    // Subcommand is required for actual operation
    let command = match &cli.command {
        Some(cmd) => cmd,
        None => {
            eprintln!("error: a subcommand is required");
            eprintln!("       use: dxfeed-cli connect <host> <port>");
            eprintln!("       use --dump-default-filter to print a starter filter config");
            eprintln!("       use --help for usage information");
            process::exit(1);
        }
    };

    // Callsign is required for connection
    let callsign = match &cli.callsign {
        Some(c) => c.clone(),
        None => {
            eprintln!("error: --callsign is required");
            process::exit(1);
        }
    };

    // Build source config
    let source_id = SourceId(cli.source_id.clone());
    let source = match command {
        Command::Connect {
            host,
            port,
            originator_policy,
        } => {
            let policy = match originator_policy.as_str() {
                "auto" => OriginatorPolicy::Auto,
                "all-skimmer" => OriginatorPolicy::AllSkimmer,
                "all-human" => OriginatorPolicy::AllHuman,
                other => {
                    eprintln!("error: unknown originator policy '{other}'");
                    eprintln!("       valid values: auto, all-skimmer, all-human");
                    process::exit(1);
                }
            };

            let mut config = ClusterSourceConfig::new(host, *port, &callsign, source_id);
            config.password = cli.password.clone();
            config.originator_policy = policy;
            SourceConfig::Cluster(config)
        }
    };

    // Load filter config
    let filter = match &cli.filter_file {
        Some(path) => {
            let data = match std::fs::read_to_string(path) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("error: cannot read filter file {}: {e}", path.display());
                    process::exit(1);
                }
            };
            match serde_json::from_str::<FilterConfigSerde>(&data) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("error: invalid filter JSON: {e}");
                    process::exit(1);
                }
            }
        }
        None => FilterConfigSerde::default(),
    };

    // Aggregator config
    let agg_config = AggregatorConfig {
        spot_ttl: Duration::from_secs(cli.spot_ttl),
        dedupe: DedupeConfig {
            emit_updates: cli.emit_updates,
        },
        ..AggregatorConfig::default()
    };

    // Build the feed
    let mut builder = DxFeedBuilder::new()
        .add_source(source)
        .set_filter(filter)
        .set_aggregator_config(agg_config);

    // Skimmer quality
    if !cli.no_skimmer_quality {
        builder = builder.set_skimmer_quality(SkimmerQualityConfig::default());
    }

    // Entity resolver (cty.dat)
    if let Some(cty_path) = &cli.cty_file {
        match dxfeed::resolver::cty::CtyResolver::from_file(cty_path.as_path()) {
            Ok(resolver) => {
                if cli.verbose {
                    eprintln!(
                        "[info] loaded cty.dat: {} entities, {} prefixes",
                        resolver.entity_count(),
                        resolver.prefix_count()
                    );
                }
                builder = builder.entity_resolver(Box::new(resolver));
            }
            Err(e) => {
                eprintln!("error: failed to load cty.dat: {e}");
                process::exit(1);
            }
        }
    }

    let mut feed = match builder.build() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: failed to build feed: {e}");
            process::exit(1);
        }
    };

    if cli.verbose {
        eprintln!("[info] feed started, waiting for events...");
    }

    // Event loop with Ctrl+C handling
    loop {
        tokio::select! {
            event = feed.next_event() => {
                match event {
                    Some(event) => handle_event(&event, &cli),
                    None => break,
                }
            }
            _ = tokio::signal::ctrl_c() => {
                if cli.verbose {
                    eprintln!("[info] shutting down...");
                }
                feed.shutdown();
                // Drain remaining events
                while let Some(event) = feed.next_event().await {
                    handle_event(&event, &cli);
                }
                break;
            }
        }
    }

    if cli.verbose {
        eprintln!("[info] done");
    }
}

// ---------------------------------------------------------------------------
// Event dispatch
// ---------------------------------------------------------------------------

fn handle_event(event: &DxEvent, cli: &Cli) {
    match event {
        DxEvent::Spot(spot_event) => {
            // Skip Update events unless --emit-updates
            if spot_event.kind == SpotEventKind::Update && !cli.emit_updates {
                return;
            }
            if cli.json {
                print_spot_json(spot_event);
            } else {
                print_spot_human(spot_event);
            }
        }
        DxEvent::Announce(ann) => {
            if cli.verbose {
                eprintln!("[announce] {}: {}", ann.source.0, ann.text);
            }
        }
        DxEvent::SourceStatus(status) => {
            if cli.verbose {
                eprintln!("[status] {}: {:?}", status.source_id.0, status.state);
            }
        }
        DxEvent::Error(err) => {
            let src = err
                .source_id
                .as_ref()
                .map(|s| s.0.as_str())
                .unwrap_or("?");
            eprintln!("[error] {src}: {}", err.message);
        }
    }
}

// ---------------------------------------------------------------------------
// JSON output
// ---------------------------------------------------------------------------

fn print_spot_json(event: &DxSpotEvent) {
    match serde_json::to_string(event) {
        Ok(json) => println!("{json}"),
        Err(e) => eprintln!("[error] JSON serialization failed: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Human-readable output
// ---------------------------------------------------------------------------

fn print_spot_human(event: &DxSpotEvent) {
    let kind_tag = match event.kind {
        SpotEventKind::New => "NEW",
        SpotEventKind::Update => "UPD",
        SpotEventKind::Withdraw => "WDR",
    };

    let spot = &event.spot;

    if event.kind == SpotEventKind::Withdraw {
        println!(
            "{:<3}  {:>9} {:<3} {:<4} {:<13}  {:>42} r{}",
            kind_tag,
            format_freq(spot.freq_hz),
            format_mode(spot.mode),
            format_band(spot.band),
            spot.dx_call,
            "",
            event.revision,
        );
        return;
    }

    let comment = spot.comment.as_deref().unwrap_or("");
    let obs_count = spot.observations.len();
    let orig_count = spot.unique_originators;
    let quality = format_quality(spot.skim_quality);
    let last_spotter = spot
        .observations
        .last()
        .map(|o| o.originator.0.as_str())
        .unwrap_or("-");

    println!(
        "{:<3}  {:>9} {:<3} {:<4} {:<13} {:<12} {:<20} {}o {}s {} r{}",
        kind_tag,
        format_freq(spot.freq_hz),
        format_mode(spot.mode),
        format_band(spot.band),
        spot.dx_call,
        truncate(last_spotter, 12),
        truncate(comment, 20),
        obs_count,
        orig_count,
        quality,
        event.revision,
    );
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

fn format_freq(hz: u64) -> String {
    let khz = hz / 1000;
    let frac = (hz % 1000) / 100;
    format!("{khz}.{frac}")
}

fn format_mode(mode: dxfeed::domain::DxMode) -> &'static str {
    use dxfeed::domain::DxMode;
    match mode {
        DxMode::CW => "CW",
        DxMode::SSB => "SSB",
        DxMode::DIG => "DIG",
        DxMode::AM => "AM",
        DxMode::FM => "FM",
        DxMode::Unknown => "?",
    }
}

fn format_band(band: dxfeed::domain::Band) -> &'static str {
    use dxfeed::domain::Band;
    match band {
        Band::B160 => "160m",
        Band::B80 => "80m",
        Band::B60 => "60m",
        Band::B40 => "40m",
        Band::B30 => "30m",
        Band::B20 => "20m",
        Band::B17 => "17m",
        Band::B15 => "15m",
        Band::B12 => "12m",
        Band::B10 => "10m",
        Band::B6 => "6m",
        Band::B2 => "2m",
        Band::Unknown => "?",
    }
}

fn format_quality(quality: Option<SkimQualityTag>) -> &'static str {
    match quality {
        Some(SkimQualityTag::Valid) => "Valid",
        Some(SkimQualityTag::Busted) => "Busted",
        Some(SkimQualityTag::Qsy) => "Qsy",
        Some(SkimQualityTag::Unknown) => "Unk",
        None => "-",
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}
