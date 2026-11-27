//! dxfeed-record — raw DX cluster capture for offline replay.
//!
//! Connects to one or more cluster nodes, logs in, and appends every line
//! received to an hourly JSONL file per source, stamped with the UTC time the
//! bytes were read from the socket. It deliberately uses none of dxfeed's
//! parsing or login code, so a capture is independent of the library version
//! it will later be replayed through.
//!
//! Each output line is one JSON object:
//!
//! ```text
//! {"t":"2026-11-28T00:00:00.123456Z","src":"rbn","ev":"line","data":"DX de ..."}
//! ```
//!
//! `ev` is one of:
//! - `line`: a complete line from the node, without its line ending. Bytes
//!   are decoded as Latin-1 (one byte per character), so the original bytes
//!   can be recovered exactly, telnet IAC sequences included.
//! - `partial`: unterminated text that sat for a second (typically a login
//!   prompt). Informational only; the same text appears again at the start of
//!   the next `line` once the node terminates it.
//! - `sent`: text the recorder sent to the node.
//! - `connecting`, `connected`, `connect_failed`, `disconnected`: connection
//!   lifecycle, with a reason in `data` where there is one.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use clap::Parser;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use dxfeed::source::iac::strip_iac;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const LOGIN_FALLBACK: Duration = Duration::from_secs(5);
const SEND_DELAY: Duration = Duration::from_secs(3);
const PARTIAL_AFTER: Duration = Duration::from_secs(1);
const BACKOFF_INITIAL: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const STABLE_AFTER: Duration = Duration::from_secs(60);
const SYNC_EVERY: Duration = Duration::from_secs(30);
const MAX_PENDING: usize = 64 * 1024;

#[derive(Parser)]
#[command(name = "dxfeed-record", about = "Record raw DX cluster lines with receive timestamps")]
struct Cli {
    /// Callsign to log in with
    #[arg(short, long)]
    callsign: String,

    /// Node to record, as NAME=HOST:PORT (repeatable)
    #[arg(long = "source", value_name = "NAME=HOST:PORT", required = true)]
    sources: Vec<String>,

    /// Command to send to a node after login, as NAME=TEXT (repeatable)
    #[arg(long = "send", value_name = "NAME=TEXT")]
    sends: Vec<String>,

    /// Directory for capture files
    #[arg(short, long, default_value = "capture")]
    out_dir: PathBuf,

    /// Reconnect if a node sends nothing for this many seconds
    #[arg(long, default_value = "300")]
    silence_timeout: u64,

    /// Stop recording at this UTC time (RFC 3339, e.g. 2026-11-29T00:05:00Z)
    #[arg(long)]
    stop_at: Option<DateTime<Utc>>,
}

struct Source {
    name: String,
    addr: String,
    sends: Vec<String>,
}

fn parse_sources(cli: &Cli) -> Result<Vec<Source>, String> {
    let mut sources: Vec<Source> = Vec::new();
    for spec in &cli.sources {
        let (name, addr) = spec
            .split_once('=')
            .ok_or_else(|| format!("--source '{spec}': expected NAME=HOST:PORT"))?;
        if name.is_empty()
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(format!("--source '{spec}': name must be letters, digits, '_' or '-'"));
        }
        let port_ok = addr
            .rsplit_once(':')
            .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok());
        if !port_ok {
            return Err(format!("--source '{spec}': expected HOST:PORT after '='"));
        }
        if sources.iter().any(|s| s.name == name) {
            return Err(format!("--source '{spec}': duplicate name '{name}'"));
        }
        sources.push(Source { name: name.to_string(), addr: addr.to_string(), sends: Vec::new() });
    }
    for spec in &cli.sends {
        let (name, text) = spec
            .split_once('=')
            .ok_or_else(|| format!("--send '{spec}': expected NAME=TEXT"))?;
        let source = sources
            .iter_mut()
            .find(|s| s.name == name)
            .ok_or_else(|| format!("--send '{spec}': no source named '{name}'"))?;
        source.sends.push(text.to_string());
    }
    Ok(sources)
}

// ---------------------------------------------------------------------------
// Capture file writer
// ---------------------------------------------------------------------------

/// Appends events to `<dir>/<name>-<YYYYMMDD>-<HH>Z.jsonl`, one file per UTC
/// hour. Write errors are reported and the file is reopened on the next
/// event, so a transient disk problem does not end the capture.
struct Recorder {
    dir: PathBuf,
    name: String,
    file: Option<(String, BufWriter<File>)>,
    last_sync: Instant,
    lines: Arc<AtomicU64>,
}

impl Recorder {
    fn new(dir: PathBuf, name: String, lines: Arc<AtomicU64>) -> Self {
        Self { dir, name, file: None, last_sync: Instant::now(), lines }
    }

    fn event(&mut self, t: DateTime<Utc>, ev: &str, data: Option<&str>) {
        let mut out = format!(
            "{{\"t\":\"{}\",\"src\":\"{}\",\"ev\":\"{}\"",
            t.to_rfc3339_opts(SecondsFormat::Micros, true),
            self.name,
            ev
        );
        if let Some(data) = data {
            out.push_str(",\"data\":");
            out.push_str(&serde_json::to_string(data).unwrap_or_else(|_| "\"\"".into()));
        }
        out.push_str("}\n");

        if let Err(e) = self.write(t, out.as_bytes()) {
            eprintln!("[{}] write failed: {e}", self.name);
            self.file = None;
        }
        if ev != "line" && ev != "partial" {
            eprintln!("[{}] {ev}{}", self.name, data.map(|d| format!(": {d}")).unwrap_or_default());
        }
    }

    fn line(&mut self, t: DateTime<Utc>, ev: &str, bytes: &[u8]) {
        let mut end = bytes.len();
        while end > 0 && (bytes[end - 1] == b'\n' || bytes[end - 1] == b'\r') {
            end -= 1;
        }
        // Latin-1: one byte per char, so the bytes are recoverable.
        let text: String = bytes[..end].iter().map(|&b| b as char).collect();
        self.event(t, ev, Some(&text));
        if ev == "line" {
            self.lines.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn write(&mut self, t: DateTime<Utc>, bytes: &[u8]) -> std::io::Result<()> {
        let hour = t.format("%Y%m%d-%H").to_string();
        if self.file.as_ref().is_none_or(|(h, _)| *h != hour) {
            if let Some((_, mut old)) = self.file.take() {
                old.flush()?;
                old.get_ref().sync_data()?;
            }
            let path = self.dir.join(format!("{}-{hour}Z.jsonl", self.name));
            let file = OpenOptions::new().create(true).append(true).open(path)?;
            self.file = Some((hour, BufWriter::new(file)));
        }
        let (_, writer) = self.file.as_mut().expect("file opened above");
        writer.write_all(bytes)
    }

    /// Flush buffered events to the OS, and to disk every `SYNC_EVERY`.
    fn flush(&mut self) {
        let Some((_, writer)) = self.file.as_mut() else { return };
        let mut result = writer.flush();
        if result.is_ok() && self.last_sync.elapsed() >= SYNC_EVERY {
            result = writer.get_ref().sync_data();
            self.last_sync = Instant::now();
        }
        if let Err(e) = result {
            eprintln!("[{}] flush failed: {e}", self.name);
            self.file = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Connection handling
// ---------------------------------------------------------------------------

/// True if `bytes` looks like a node asking for a callsign.
fn is_login_prompt(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    let text = text.trim_end();
    text.ends_with("login:")
        || text.ends_with("call:")
        || text.ends_with("callsign:")
        || text.contains("enter your call")
}

async fn send(stream: &mut TcpStream, rec: &mut Recorder, text: &str) -> std::io::Result<()> {
    stream.write_all(format!("{text}\r\n").as_bytes()).await?;
    rec.event(Utc::now(), "sent", Some(text));
    Ok(())
}

/// Run one connection until it ends. Returns the reason it ended.
async fn session(
    mut stream: TcpStream,
    source: &Source,
    callsign: &str,
    silence_timeout: Duration,
    rec: &mut Recorder,
    shutdown: &CancellationToken,
) -> String {
    let connected_at = Instant::now();
    let mut last_data = Instant::now();
    let mut pending: Vec<u8> = Vec::new();
    let mut pending_since = Instant::now();
    let mut pending_reported = false;
    let mut login_at: Option<Instant> = None;
    let mut sends_done = source.sends.is_empty();
    let mut chunk = vec![0u8; 16 * 1024];
    let mut tick = tokio::time::interval(Duration::from_secs(1));

    loop {
        let mut want_login = false;

        tokio::select! {
            result = stream.read(&mut chunk) => {
                let n = match result {
                    Ok(0) => return "closed by node".into(),
                    Ok(n) => n,
                    Err(e) => return format!("read error: {e}"),
                };
                let t = Utc::now();
                last_data = Instant::now();

                // Refuse any telnet option negotiation, as dxfeed does.
                if chunk[..n].contains(&0xFF) {
                    let mut copy = chunk[..n].to_vec();
                    for response in &strip_iac(&mut copy).responses {
                        if let Err(e) = stream.write_all(response).await {
                            return format!("write error: {e}");
                        }
                    }
                }

                if pending.is_empty() {
                    pending_since = Instant::now();
                    pending_reported = false;
                }
                pending.extend_from_slice(&chunk[..n]);
                while let Some(pos) = pending.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=pos).collect();
                    want_login |= is_login_prompt(&line);
                    rec.line(t, "line", &line);
                    pending_since = Instant::now();
                    pending_reported = false;
                }
                if pending.len() > MAX_PENDING {
                    rec.line(t, "line", &pending);
                    pending.clear();
                }
                want_login |= is_login_prompt(&pending);
            }
            _ = tick.tick() => {
                if !pending.is_empty() && !pending_reported && pending_since.elapsed() >= PARTIAL_AFTER {
                    rec.line(Utc::now(), "partial", &pending);
                    pending_reported = true;
                }
                want_login = connected_at.elapsed() >= LOGIN_FALLBACK;
                if last_data.elapsed() >= silence_timeout {
                    return format!("no data for {} s", silence_timeout.as_secs());
                }
                rec.flush();
            }
            _ = shutdown.cancelled() => return "shutdown".into(),
        }

        if want_login && login_at.is_none() {
            if let Err(e) = send(&mut stream, rec, callsign).await {
                return format!("write error: {e}");
            }
            login_at = Some(Instant::now());
        }
        if !sends_done && login_at.is_some_and(|at| at.elapsed() >= SEND_DELAY) {
            for text in &source.sends {
                if let Err(e) = send(&mut stream, rec, text).await {
                    return format!("write error: {e}");
                }
            }
            sends_done = true;
        }
    }
}

async fn run_source(
    source: Source,
    callsign: String,
    out_dir: PathBuf,
    silence_timeout: Duration,
    lines: Arc<AtomicU64>,
    shutdown: CancellationToken,
) {
    let mut rec = Recorder::new(out_dir, source.name.clone(), lines);
    let mut backoff = BACKOFF_INITIAL;

    while !shutdown.is_cancelled() {
        rec.event(Utc::now(), "connecting", Some(&source.addr));
        let connect = tokio::select! {
            r = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&source.addr)) => r,
            _ = shutdown.cancelled() => break,
        };
        match connect {
            Ok(Ok(stream)) => {
                rec.event(Utc::now(), "connected", None);
                let started = Instant::now();
                let reason =
                    session(stream, &source, &callsign, silence_timeout, &mut rec, &shutdown).await;
                rec.event(Utc::now(), "disconnected", Some(&reason));
                if started.elapsed() >= STABLE_AFTER {
                    backoff = BACKOFF_INITIAL;
                }
            }
            Ok(Err(e)) => rec.event(Utc::now(), "connect_failed", Some(&e.to_string())),
            Err(_) => rec.event(Utc::now(), "connect_failed", Some("connect timed out")),
        }
        rec.flush();

        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = shutdown.cancelled() => break,
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
    rec.flush();
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let sources = match parse_sources(&cli) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = std::fs::create_dir_all(&cli.out_dir) {
        eprintln!("error: cannot create {}: {e}", cli.out_dir.display());
        std::process::exit(1);
    }

    let shutdown = CancellationToken::new();
    let mut counters: Vec<(String, Arc<AtomicU64>)> = Vec::new();
    let mut tasks = Vec::new();
    for source in sources {
        let lines = Arc::new(AtomicU64::new(0));
        counters.push((source.name.clone(), lines.clone()));
        tasks.push(tokio::spawn(run_source(
            source,
            cli.callsign.clone(),
            cli.out_dir.clone(),
            Duration::from_secs(cli.silence_timeout),
            lines,
            shutdown.clone(),
        )));
    }
    eprintln!("recording to {} (callsign {})", cli.out_dir.display(), cli.callsign);

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install SIGTERM handler");
    let mut stats = tokio::time::interval(Duration::from_secs(60));
    stats.tick().await;
    let mut previous: Vec<u64> = vec![0; counters.len()];

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => { eprintln!("interrupted; stopping"); break; }
            _ = sigterm.recv() => { eprintln!("terminated; stopping"); break; }
            _ = stats.tick() => {
                let mut report = String::new();
                for ((name, counter), prev) in counters.iter().zip(previous.iter_mut()) {
                    let total = counter.load(Ordering::Relaxed);
                    report.push_str(&format!("  {name}: {total} lines (+{}/min)", total - *prev));
                    *prev = total;
                }
                eprintln!("{}{report}", Utc::now().format("%H:%M:%SZ"));
                if cli.stop_at.is_some_and(|at| Utc::now() >= at) {
                    eprintln!("reached --stop-at; stopping");
                    break;
                }
            }
        }
    }

    shutdown.cancel();
    for task in tasks {
        let _ = task.await;
    }
}
