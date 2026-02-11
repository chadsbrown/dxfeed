//! dxfeed-cli — TUI for the dxfeed library.
//!
//! Connects to a DX cluster node and displays aggregated spots in a terminal
//! user interface with activity feed and spots table views.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::process;
use std::time::Duration;

use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use crossterm::event::{
    self, Event, EventStream, KeyCode, KeyEvent, KeyModifiers,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use ratatui::Terminal;

use dxfeed::aggregator::core::{AggregatorConfig, DedupeConfig};
use dxfeed::domain::{Band, DxMode};
use dxfeed::feed::DxFeedBuilder;
use dxfeed::filter::config::FilterConfigSerde;
use dxfeed::model::{
    DxEvent, DxSpot, DxSpotEvent, SourceConnectionState, SourceId,
    SpotEventKind, SpotKey,
};
use dxfeed::skimmer::config::SkimmerQualityConfig;
use dxfeed::source::cluster::{ClusterSourceConfig, OriginatorPolicy};
use dxfeed::source::supervisor::SourceConfig;

// ---------------------------------------------------------------------------
// CLI argument definitions
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "dxfeed-cli", about = "DX cluster spot aggregator TUI")]
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

    /// Disable skimmer quality gating.
    #[arg(long, global = true)]
    no_skimmer_quality: bool,

    /// Spot TTL in seconds.
    #[arg(long, default_value = "900", global = true)]
    spot_ttl: u64,

    /// Output events as JSON (one object per line, non-TUI mode).
    #[arg(long, global = true)]
    json: bool,

    /// Print status/announce events to stderr (JSON mode only).
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
// App state
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Activity,
    Spots,
}

struct ActivityEntry {
    timestamp: DateTime<Utc>,
    kind: SpotEventKind,
    freq_hz: u64,
    mode: DxMode,
    band: Band,
    dx_call: String,
    last_spotter: String,
    obs_count: usize,
    unique_originators: u8,
}

struct Stats {
    new_count: u64,
    update_count: u64,
    withdraw_count: u64,
}

struct App {
    active_spots: HashMap<SpotKey, DxSpot>,
    activity: VecDeque<ActivityEntry>,
    stats: Stats,
    view: View,
    scroll_offset: usize,
    source_states: HashMap<String, SourceConnectionState>,
    should_quit: bool,
}

const MAX_ACTIVITY: usize = 500;
const SPOT_MAX_AGE: Duration = Duration::from_secs(300); // 5 minutes

impl App {
    fn new() -> Self {
        Self {
            active_spots: HashMap::new(),
            activity: VecDeque::new(),
            stats: Stats {
                new_count: 0,
                update_count: 0,
                withdraw_count: 0,
            },
            view: View::Activity,
            scroll_offset: 0,
            source_states: HashMap::new(),
            should_quit: false,
        }
    }

    fn handle_event(&mut self, event: &DxEvent) {
        match event {
            DxEvent::Spot(spot_event) => self.handle_spot(spot_event),
            DxEvent::SourceStatus(status) => {
                self.source_states
                    .insert(status.source_id.0.clone(), status.state.clone());
            }
            DxEvent::Announce(_) | DxEvent::Error(_) => {}
        }
    }

    fn handle_spot(&mut self, event: &DxSpotEvent) {
        let spot = &event.spot;

        // Push activity entry
        let entry = ActivityEntry {
            timestamp: Utc::now(),
            kind: event.kind,
            freq_hz: spot.freq_hz,
            mode: spot.mode,
            band: spot.band,
            dx_call: spot.dx_call.clone(),
            last_spotter: spot
                .observations
                .last()
                .map(|o| o.originator.0.clone())
                .unwrap_or_default(),
            obs_count: spot.observations.len(),
            unique_originators: spot.unique_originators,
        };
        self.activity.push_back(entry);
        if self.activity.len() > MAX_ACTIVITY {
            self.activity.pop_front();
        }

        match event.kind {
            SpotEventKind::New => {
                self.stats.new_count += 1;
                self.active_spots
                    .insert(spot.spot_key.clone(), spot.clone());
            }
            SpotEventKind::Update => {
                self.stats.update_count += 1;
                self.active_spots
                    .insert(spot.spot_key.clone(), spot.clone());
            }
            SpotEventKind::Withdraw => {
                self.stats.withdraw_count += 1;
                self.active_spots.remove(&spot.spot_key);
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Tab => {
                self.view = match self.view {
                    View::Activity => View::Spots,
                    View::Spots => View::Activity,
                };
                self.scroll_offset = 0;
            }
            KeyCode::Up => {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
            }
            KeyCode::Down => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
            }
            KeyCode::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_add(20);
            }
            KeyCode::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(20);
            }
            KeyCode::Home => {
                self.scroll_offset = usize::MAX; // will be clamped in render
            }
            KeyCode::End => {
                self.scroll_offset = 0;
            }
            _ => {}
        }
    }

    fn evict_stale_spots(&mut self) {
        let cutoff = Utc::now() - SPOT_MAX_AGE;
        self.active_spots.retain(|_, spot| spot.last_seen > cutoff);
    }
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
        let json =
            serde_json::to_string_pretty(&cfg).expect("serialize default filter");
        println!("{json}");
        return;
    }

    // Subcommand is required for actual operation
    let command = match &cli.command {
        Some(cmd) => cmd,
        None => {
            eprintln!("error: a subcommand is required");
            eprintln!("       use: dxfeed-cli connect <host> <port>");
            eprintln!(
                "       use --dump-default-filter to print a starter filter config"
            );
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
                    eprintln!(
                        "       valid values: auto, all-skimmer, all-human"
                    );
                    process::exit(1);
                }
            };

            let mut config =
                ClusterSourceConfig::new(host, *port, &callsign, source_id);
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
                    eprintln!(
                        "error: cannot read filter file {}: {e}",
                        path.display()
                    );
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

    // Aggregator config — TUI always wants updates for state tracking
    let agg_config = AggregatorConfig {
        spot_ttl: Duration::from_secs(cli.spot_ttl),
        dedupe: DedupeConfig {
            emit_updates: true,
            min_update_interval: Some(Duration::from_secs(45)),
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
        match dxfeed::resolver::cty::CtyResolver::from_file(cty_path.as_path())
        {
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

    // Branch: JSON passthrough mode vs TUI mode
    if cli.json {
        run_json_mode(&mut feed, &cli).await;
    } else {
        if let Err(e) = run_tui(&mut feed).await {
            eprintln!("error: TUI failed: {e}");
            process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// JSON passthrough mode (non-TUI, for scripting)
// ---------------------------------------------------------------------------

async fn run_json_mode(
    feed: &mut dxfeed::feed::DxFeed,
    cli: &Cli,
) {
    loop {
        tokio::select! {
            event = feed.next_event() => {
                match event {
                    Some(event) => handle_json_event(&event, cli),
                    None => break,
                }
            }
            _ = tokio::signal::ctrl_c() => {
                if cli.verbose {
                    eprintln!("[info] shutting down...");
                }
                feed.shutdown();
                while let Some(event) = feed.next_event().await {
                    handle_json_event(&event, cli);
                }
                break;
            }
        }
    }
}

fn handle_json_event(event: &DxEvent, cli: &Cli) {
    match event {
        DxEvent::Spot(spot_event) => {
            match serde_json::to_string(spot_event) {
                Ok(json) => println!("{json}"),
                Err(e) => eprintln!("[error] JSON serialization failed: {e}"),
            }
        }
        DxEvent::Announce(ann) => {
            if cli.verbose {
                eprintln!("[announce] {}: {}", ann.source.0, ann.text);
            }
        }
        DxEvent::SourceStatus(status) => {
            if cli.verbose {
                eprintln!(
                    "[status] {}: {:?}",
                    status.source_id.0, status.state
                );
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
// TUI mode
// ---------------------------------------------------------------------------

async fn run_tui(
    feed: &mut dxfeed::feed::DxFeed,
) -> io::Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    let mut event_stream = EventStream::new();
    let tick_rate = Duration::from_millis(100);

    let result = run_tui_loop(&mut terminal, &mut app, feed, &mut event_stream, tick_rate).await;

    // Restore terminal
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    result
}

async fn run_tui_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    feed: &mut dxfeed::feed::DxFeed,
    event_stream: &mut EventStream,
    tick_rate: Duration,
) -> io::Result<()> {
    let mut tick = tokio::time::interval(tick_rate);

    loop {
        tokio::select! {
            event = feed.next_event() => {
                match event {
                    Some(ev) => app.handle_event(&ev),
                    None => {
                        app.should_quit = true;
                    }
                }
            }
            maybe_event = event_stream.next() => {
                if let Some(Ok(Event::Key(key))) = maybe_event {
                    // crossterm 0.28 fires Press and Release; only handle Press
                    if key.kind == event::KeyEventKind::Press {
                        app.handle_key(key);
                    }
                }
            }
            _ = tick.tick() => {
                app.evict_stale_spots();
            }
        }

        if app.should_quit {
            feed.shutdown();
            break;
        }

        terminal.draw(|f| draw(f, app))?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

fn draw(f: &mut ratatui::Frame, app: &mut App) {
    let chunks = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Min(5),   // main area
        Constraint::Length(1), // stats bar
    ])
    .split(f.area());

    draw_header(f, app, chunks[0]);
    draw_main(f, app, chunks[1]);
    draw_stats(f, app, chunks[2]);
}

fn draw_header(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let activity_style = if app.view == View::Activity {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let spots_style = if app.view == View::Spots {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    // Build source status string
    let mut source_spans: Vec<Span> = Vec::new();
    for (name, state) in &app.source_states {
        let (label, color) = match state {
            SourceConnectionState::Connected => ("Connected", Color::Green),
            SourceConnectionState::Connecting => ("Connecting", Color::Yellow),
            SourceConnectionState::Reconnecting { attempt } => {
                // Can't easily format in a const, so we handle below
                source_spans.push(Span::styled(
                    format!("  {name}: Reconn #{attempt}"),
                    Style::default().fg(Color::Yellow),
                ));
                continue;
            }
            SourceConnectionState::Failed { .. } => ("Failed", Color::Red),
            SourceConnectionState::Shutdown => ("Shutdown", Color::DarkGray),
        };
        source_spans.push(Span::styled(
            format!("  {name}: {label}"),
            Style::default().fg(color),
        ));
    }

    let mut spans = vec![
        Span::styled(" dxfeed ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(" Activity ", activity_style),
        Span::raw(" / "),
        Span::styled(" Spots ", spots_style),
    ];
    spans.extend(source_spans);

    f.render_widget(
        Paragraph::new(Line::from(spans))
            .style(Style::default().bg(Color::DarkGray)),
        area,
    );
}

fn draw_main(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    match app.view {
        View::Activity => draw_activity(f, app, area),
        View::Spots => draw_spots(f, app, area),
    }
}

fn draw_activity(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let visible_height = area.height as usize;
    let total = app.activity.len();

    // Clamp scroll_offset
    let max_scroll = total.saturating_sub(visible_height);
    if app.scroll_offset > max_scroll {
        app.scroll_offset = max_scroll;
    }

    // scroll_offset=0 means "at the bottom" (newest visible)
    let start = total.saturating_sub(visible_height + app.scroll_offset);
    let end = total.saturating_sub(app.scroll_offset);

    let lines: Vec<Line> = app
        .activity
        .iter()
        .skip(start)
        .take(end - start)
        .map(|entry| {
            let time_str = entry.timestamp.format("%H:%M:%S").to_string();
            let kind_span = match entry.kind {
                SpotEventKind::New => {
                    Span::styled(" NEW ", Style::default().fg(Color::Green))
                }
                SpotEventKind::Update => {
                    Span::styled(" UPD ", Style::default().fg(Color::Yellow))
                }
                SpotEventKind::Withdraw => {
                    Span::styled(" WDR ", Style::default().fg(Color::Red))
                }
            };

            let detail = if entry.kind == SpotEventKind::Withdraw {
                format!(
                    " {:>9} {:<3} {:<4} {:<13}",
                    format_freq(entry.freq_hz),
                    format_mode(entry.mode),
                    format_band(entry.band),
                    entry.dx_call,
                )
            } else {
                format!(
                    " {:>9} {:<3} {:<4} {:<13} {:<12} {}o {}s",
                    format_freq(entry.freq_hz),
                    format_mode(entry.mode),
                    format_band(entry.band),
                    entry.dx_call,
                    truncate(&entry.last_spotter, 12),
                    entry.obs_count,
                    entry.unique_originators,
                )
            };

            Line::from(vec![
                Span::styled(time_str, Style::default().fg(Color::DarkGray)),
                kind_span,
                Span::raw(detail),
            ])
        })
        .collect();

    let scroll_indicator = if app.scroll_offset > 0 {
        format!(" [+{}] ", app.scroll_offset)
    } else {
        String::new()
    };

    let block = Block::default()
        .borders(Borders::NONE)
        .title(format!("Activity ({}){scroll_indicator}", total));

    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_spots(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let header_height: usize = 2; // header row + border
    let visible_height = (area.height as usize).saturating_sub(header_height);

    // Sort spots by frequency
    let mut spots: Vec<&DxSpot> = app.active_spots.values().collect();
    spots.sort_by_key(|s| s.freq_hz);

    let total = spots.len();
    let max_scroll = total.saturating_sub(visible_height);
    if app.scroll_offset > max_scroll {
        app.scroll_offset = max_scroll;
    }

    // scroll_offset=0 means "top of list" for spots view
    let start = app.scroll_offset;
    let end = (start + visible_height).min(total);

    let now = Utc::now();

    let rows: Vec<Row> = spots[start..end]
        .iter()
        .map(|spot| {
            let first = now
                .signed_duration_since(spot.first_seen)
                .num_seconds()
                .max(0) as u64;
            let last = now
                .signed_duration_since(spot.last_seen)
                .num_seconds()
                .max(0) as u64;
            let last_spotter = spot
                .observations
                .last()
                .map(|o| o.originator.0.as_str())
                .unwrap_or("-");
            let comment = spot.comment.as_deref().unwrap_or("");

            Row::new(vec![
                Cell::from(format_freq(spot.freq_hz)),
                Cell::from(format_mode(spot.mode)),
                Cell::from(format_band(spot.band)),
                Cell::from(spot.dx_call.as_str().to_owned()),
                Cell::from(truncate(last_spotter, 12).to_owned()),
                Cell::from(format_age(first)),
                Cell::from(format_age(last)),
                Cell::from(format!("{}", spot.observations.len())),
                Cell::from(truncate(comment, 24).to_owned()),
            ])
        })
        .collect();

    let header = Row::new(vec![
        Cell::from("Freq").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Mode").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Band").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("DX Call").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Spotter").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("First").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Last").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Obs").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Comment").style(Style::default().add_modifier(Modifier::BOLD)),
    ])
    .style(Style::default().fg(Color::Cyan));

    let widths = [
        Constraint::Length(10),
        Constraint::Length(4),
        Constraint::Length(5),
        Constraint::Length(14),
        Constraint::Length(13),
        Constraint::Length(7),
        Constraint::Length(7),
        Constraint::Length(4),
        Constraint::Fill(1),
    ];

    let scroll_indicator = if app.scroll_offset > 0 {
        format!(" [+{}] ", app.scroll_offset)
    } else {
        String::new()
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::NONE)
                .title(format!("Spots ({}){scroll_indicator}", total)),
        )
        .row_highlight_style(Style::default().add_modifier(Modifier::BOLD));

    f.render_widget(table, area);
}

fn draw_stats(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let active = app.active_spots.len();
    let new = app.stats.new_count;
    let upd = app.stats.update_count;
    let wdr = app.stats.withdraw_count;

    let line = Line::from(vec![
        Span::styled(" New: ", Style::default().fg(Color::Green)),
        Span::raw(format!("{new}")),
        Span::styled("  Upd: ", Style::default().fg(Color::Yellow)),
        Span::raw(format!("{upd}")),
        Span::styled("  Wdr: ", Style::default().fg(Color::Red)),
        Span::raw(format!("{wdr}")),
        Span::styled("  Active: ", Style::default().fg(Color::Cyan)),
        Span::raw(format!("{active}")),
        Span::raw("  "),
        Span::styled(
            "Tab:view  q:quit  \u{2191}\u{2193}/PgUp/PgDn:scroll",
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(Color::DarkGray)),
        area,
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

fn format_mode(mode: DxMode) -> &'static str {
    match mode {
        DxMode::CW => "CW",
        DxMode::SSB => "SSB",
        DxMode::DIG => "DIG",
        DxMode::AM => "AM",
        DxMode::FM => "FM",
        DxMode::Unknown => "?",
    }
}

fn format_band(band: Band) -> &'static str {
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

fn format_age(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        let m = secs / 60;
        let s = secs % 60;
        format!("{m}m{s:02}s")
    } else {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        format!("{h}h{m:02}m")
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}
