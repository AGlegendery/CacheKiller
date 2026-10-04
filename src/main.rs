//! CacheKiller — fast, accurate TUI cache cleaner for Linux and Windows.

mod app;
mod delete;
mod locations;
mod log;
mod platform;
mod scan;
mod ui;
mod util;

use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyEventKind};

use crate::app::App;
use crate::scan::Progress;
use crate::platform::Ctx;
use crate::util::{fmt_age, fmt_count, fmt_size, now_secs, tilde};

#[cfg(unix)]
const HELP: &str = "\
CacheKiller — fast, accurate cache cleaner

USAGE:
    cachekiller [OPTIONS]
    sudo cachekiller          # also clean system caches (apt, logs, journal, crash dumps)
                              # (or press E inside the TUI)

OPTIONS:
    -l, --list             Scan and print a report instead of opening the TUI
        --min-age <HOURS>  Leave /tmp and /var/tmp entries changed within HOURS alone [default: 24]
    -h, --help             Show this help
    -V, --version          Show version

Logs: $XDG_STATE_HOME/cachekiller/{delete.log,failed.log}";

#[cfg(windows)]
const HELP: &str = "\
CacheKiller — fast, accurate cache cleaner

USAGE:
    cachekiller.exe [OPTIONS]
    Run as Administrator (or press E inside the TUI) to also clean system caches
    (Windows Update, Windows\\Temp, logs, crash dumps).

OPTIONS:
    -l, --list             Scan and print a report instead of opening the TUI
        --min-age <HOURS>  Leave %TEMP% entries changed within HOURS alone [default: 24]
    -h, --help             Show this help
    -V, --version          Show version

Logs: %LOCALAPPDATA%\\CacheKiller\\{delete.log,failed.log}";

struct Args {
    list: bool,
    min_age_hours: i64,
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = Args { list: false, min_age_hours: 24 };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-l" | "--list" => args.list = true,
            "--min-age" => {
                let v = it.next().ok_or("--min-age needs a value")?;
                args.min_age_hours = v.parse().map_err(|_| format!("invalid --min-age: {v}"))?;
            }
            s if s.starts_with("--min-age=") => {
                let v = &s["--min-age=".len()..];
                args.min_age_hours = v.parse().map_err(|_| format!("invalid --min-age: {v}"))?;
            }
            "-h" | "--help" => {
                println!("{HELP}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("cachekiller {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            other => return Err(format!("unknown argument: {other} (see --help)")),
        }
    }
    Ok(Some(args))
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(Some(a)) => a,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cachekiller: {e}");
            return ExitCode::from(2);
        }
    };
    let ctx = platform::detect_ctx(args.min_age_hours.max(0) * 3600);

    if args.list || !io::stdout().is_terminal() {
        list_report(&ctx);
        return ExitCode::SUCCESS;
    }

    match run_tui(ctx) {
        Ok((freed, elevate)) => {
            if freed > 0 {
                println!("CacheKiller closed. Freed this session: {}", fmt_size(freed));
            }
            if elevate {
                return relaunch_as_root();
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cachekiller: {e}");
            ExitCode::FAILURE
        }
    }
}

fn relaunch_as_root() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let err = platform::relaunch_elevated(&args);
    eprintln!("cachekiller: could not elevate: {err}");
    ExitCode::FAILURE
}

fn run_tui(ctx: Ctx) -> io::Result<(u64, bool)> {
    let mut terminal = ratatui::init();
    let mut app = App::new(ctx);
    let result = (|| -> io::Result<()> {
        while !app.quit {
            app.poll_jobs();
            terminal.draw(|f| ui::draw(f, &mut app))?;
            let wait = if app.busy() { Duration::from_millis(50) } else { Duration::from_millis(500) };
            if event::poll(wait)?
                && let Event::Key(k) = event::read()?
                && k.kind == KeyEventKind::Press
            {
                app.on_key(k);
            }
        }
        Ok(())
    })();
    ratatui::restore();
    result.map(|()| (app.freed_session, app.elevate))
}

fn list_report(ctx: &Ctx) {
    let prog = Progress::default();
    let tty = io::stderr().is_terminal();
    let res = std::thread::scope(|s| {
        let h = s.spawn(|| scan::scan(ctx, &prog));
        while tty && !h.is_finished() {
            eprint!(
                "\r\x1b[2K  scanning {}/{} locations · {} files · {}",
                prog.done.load(Ordering::Relaxed),
                prog.total.load(Ordering::Relaxed),
                fmt_count(prog.files.load(Ordering::Relaxed)),
                fmt_size(prog.bytes.load(Ordering::Relaxed)),
            );
            let _ = io::stderr().flush();
            std::thread::sleep(Duration::from_millis(60));
        }
        h.join().expect("scan thread panicked")
    });
    if tty {
        eprint!("\r\x1b[2K");
    }

    let now = now_secs();
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{:>10}  {:>10}  {:<8} {:<4} {:>5}  PATH", "RECLAIM", "DISK", "CATEGORY", "TYPE", "AGE");
    for it in &res.items {
        let note = it.note.as_ref().map(|n| format!("  ({n})")).unwrap_or_default();
        let lock = if it.stats.locked > 0 { "  [locked: needs root]" } else { "" };
        let _ = writeln!(
            out,
            "{:>10}  {:>10}  {:<8} {:<4} {:>5}  {}{note}{lock}",
            fmt_size(it.stats.reclaimable()),
            fmt_size(it.stats.disk),
            it.category.name(),
            it.kind.name(),
            fmt_age(now - it.stats.newest),
            tilde(&it.path, &ctx.home),
        );
    }
    let total: u64 = res.items.iter().map(|i| i.stats.reclaimable()).sum();
    let locked: u64 = res.items.iter().map(|i| i.stats.locked).sum();
    let shared: u64 = res.items.iter().map(|i| i.stats.shared).sum();
    let secs = res.elapsed.as_secs_f64();
    let _ = writeln!(
        out,
        "\n{} items · reclaimable {} · locked {} · hard-linked {} · {} files in {:.2}s ({}/s)",
        res.items.len(),
        fmt_size(total),
        fmt_size(locked),
        fmt_size(shared),
        fmt_count(res.files),
        secs,
        fmt_count((res.files as f64 / secs.max(0.001)) as u64),
    );
    if res.hidden_recent_tmp > 0 {
        let _ = writeln!(out, "{} temp entries changed within {} were left out", res.hidden_recent_tmp, fmt_age(ctx.tmp_min_age_secs));
    }
}
