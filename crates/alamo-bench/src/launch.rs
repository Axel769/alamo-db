//! `alamo-bench launch`: the numbers for the launch, through the path the
//! app really takes. Not the M0 driver loop: a `Session` (§7.2) with the
//! engine's driver, the user's `SELECT *` streamed into the columnar result
//! set (§4), then the windows the grid asks for while scrolling (§8).
//!
//! Per table, `reps` runs, median:
//! - **first rows**: from `execute` to the first closed chunk, what the user
//!   waits before the grid shows something;
//! - **all rows**: to the end of the stream, and rows/s;
//! - **memory**: bytes of the result set as the layout counts them (§4.8)
//!   and the peak RSS of the process, so the overhead beyond the data
//!   shows. The peak is the process's: one table per invocation for a
//!   clean figure (the previous result set is released before each run,
//!   like a closed tab);
//! - **scroll window**: 60 rows × all columns at random positions, the
//!   request of one grid repaint, p50/p99 over `WINDOWS` samples.
//!
//! Byte budget and row pause are off: the measure is the whole stream, not
//! the §5 pause (which the app would show at 384 MB).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alamo_core::stream::FetchOptions;
use alamo_session::events::{ExecEvent, OutcomeOut};
use alamo_session::Session;

use crate::dataset::Table;
use crate::rss;

/// Rows of one repaint of the grid on a tall screen.
const WINDOW_ROWS: u64 = 60;
const WINDOWS: usize = 2000;

#[derive(Default)]
struct Run {
    first_rows: Option<Duration>,
    total: Option<Duration>,
    result_id: Option<u64>,
    rows: u64,
    layout_bytes: u64,
    error: Option<String>,
    done: bool,
}

/// One execution of `sql` to the end, timing the events as they arrive.
async fn execute(session: &Session, sql: &str) -> Result<Run, String> {
    let run = Arc::new(Mutex::new(Run::default()));
    let started = Instant::now();
    let sink = Arc::clone(&run);
    let opts = FetchOptions {
        soft_budget_bytes: None,
        pause_after_rows: None,
        ..FetchOptions::default()
    };
    session
        .execute(sql.to_owned(), opts, move |ev| {
            let mut r = sink.lock().unwrap();
            match ev {
                ExecEvent::Rows {
                    result_id,
                    rows,
                    layout_bytes,
                    ..
                } => {
                    r.first_rows.get_or_insert_with(|| started.elapsed());
                    r.result_id = Some(result_id);
                    r.rows = rows;
                    r.layout_bytes = layout_bytes;
                }
                ExecEvent::StatementFinished { outcome, .. } => match outcome {
                    OutcomeOut::RowSet {
                        result_id, rows, ..
                    } => {
                        r.result_id = Some(result_id);
                        r.rows = rows;
                    }
                    OutcomeOut::Failed { error } => r.error = Some(error.message),
                    _ => {}
                },
                ExecEvent::Finished { .. } => {
                    r.total = Some(started.elapsed());
                    r.done = true;
                }
                _ => {}
            }
        })
        .map_err(|e| e.message.clone())?;
    while !run.lock().unwrap().done {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let mut r = run.lock().unwrap();
    match r.error.take() {
        Some(e) => Err(e),
        None => Ok(std::mem::take(&mut *r)),
    }
}

/// Runs statements one after the other (dataset preparation).
pub async fn run_sql(session: &Session, sql: &str) -> Result<(), String> {
    execute(session, sql).await.map(|_| ())
}

struct Measure {
    first_rows: Duration,
    total: Duration,
    rows: u64,
    layout_bytes: u64,
    window_p50: Duration,
    window_p99: Duration,
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn percentile(v: &mut [Duration], p: f64) -> Duration {
    v.sort();
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

/// xorshift: random scroll positions without a dependency, the same
/// sequence at every run.
fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

async fn measure(session: &Session, table: Table, reps: usize) -> Result<Measure, String> {
    let sql = table.select_sql();
    let mut firsts = Vec::new();
    let mut totals = Vec::new();
    let mut kept: Option<Run> = None;
    for _ in 0..reps {
        // The previous result set released first, like a closed tab: the
        // peak RSS then holds one result set, not `reps` of them.
        if let Some(id) = kept.take().and_then(|r| r.result_id) {
            session.release_result(id);
        }
        let run = execute(session, &sql).await?;
        firsts.push(run.first_rows.ok_or("no rows")?);
        totals.push(run.total.ok_or("not finished")?);
        kept = Some(run);
    }
    let run = kept.ok_or("no run")?;
    let (rows, layout_bytes) = (run.rows, run.layout_bytes);
    let result = session
        .result(run.result_id.ok_or("no result set")?)
        .ok_or("result set gone")?;
    let cols = table.column_count();
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut windows = Vec::with_capacity(WINDOWS);
    for _ in 0..WINDOWS {
        let start = next(&mut state) % rows.saturating_sub(WINDOW_ROWS).max(1);
        let t = Instant::now();
        let w = result.window(start..start + WINDOW_ROWS, 0..cols);
        windows.push(t.elapsed());
        std::hint::black_box(w);
    }
    Ok(Measure {
        first_rows: median(firsts),
        total: median(totals),
        rows,
        layout_bytes,
        window_p50: percentile(&mut windows, 0.5),
        window_p99: percentile(&mut windows, 0.99),
    })
}

fn ms(d: Duration) -> String {
    format!("{:.0} ms", d.as_secs_f64() * 1000.0)
}

fn us(d: Duration) -> String {
    format!("{:.0} µs", d.as_secs_f64() * 1e6)
}

pub async fn run(url: &str, tables: &[Table], reps: usize) -> Result<(), String> {
    let session = Session::connect(url)
        .await
        .map_err(|e| format!("connection failed: {}", e.message))?;
    let engine = session.engine_name();
    let version = session.server_version().unwrap_or_default();
    println!("## {engine} {version}, {reps} runs per table, median\n");
    println!("| Table | Rows | First rows | All rows | Rows/s | Result set | Peak RSS | Scroll window p50 / p99 |");
    println!("|---|---|---|---|---|---|---|---|");
    for &t in tables {
        let m = measure(&session, t, reps).await?;
        println!(
            "| {} | {} | {} | {} | {:.0}k | {} ({} B/row) | {} | {} / {} |",
            t.label(),
            m.rows,
            ms(m.first_rows),
            ms(m.total),
            m.rows as f64 / m.total.as_secs_f64() / 1000.0,
            rss::format_bytes(m.layout_bytes),
            m.layout_bytes / m.rows.max(1),
            rss::peak_rss().map_or("n/a".into(), rss::format_bytes),
            us(m.window_p50),
            us(m.window_p99),
        );
    }
    Ok(())
}
