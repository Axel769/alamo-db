//! # alamo-bench — M0 validation spike
//!
//! §15: **before everything else.** Phase 1 of the spike, the one without a webview.
//!
//! The method matters as much as the numbers:
//!
//!  1. **fixed canonical schema** (`dataset.rs`) — "1M rows" is not a
//!     benchmark;
//!  2. **measure the ceiling first** with a "read and discard" that builds
//!     nothing. The real metric is the **distance from the ceiling**, not the
//!     absolute value: if local PG delivers 250 MB/s, the decoder will never
//!     go beyond that;
//!  3. **fixed environment** — release build with LTO, same machine, local
//!     server, no other load, 5 repetitions, median.
//!
//! ```text
//! alamo-bench prepare --url postgres://… --rows 1000000
//! alamo-bench run     --url postgres://… --table t1 --reps 5
//! alamo-bench all     --url postgres://…
//! ```

mod dataset;
mod report;
mod rss;

use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};

use alamo_driver_postgres::execute::{execute_collect, measure_ceiling, FetchOptions};
use alamo_driver_postgres::PgSession;
use dataset::Table;
use report::{grade_higher_better, grade_lower_better, Band, Metric, Report};

#[derive(Parser)]
#[command(
    name = "alamo-bench",
    about = "Alamo M0 spike: measures the socket -> columnar layout pipeline, without a webview."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Creates and fills the tables of the canonical dataset.
    Prepare {
        #[arg(long, env = "ALAMO_BENCH_URL")]
        url: String,
        #[arg(long, default_value_t = 1_000_000)]
        rows: u64,
        /// t1, t2, t3 (repeatable). Default: all.
        #[arg(long)]
        table: Vec<String>,
        /// Recreates the tables even if they exist.
        #[arg(long)]
        force: bool,
    },
    /// Measures one table.
    Run {
        #[arg(long, env = "ALAMO_BENCH_URL")]
        url: String,
        #[arg(long)]
        table: String,
        #[arg(long, default_value_t = 5)]
        reps: usize,
        /// Disables the memory soft budget.
        ///
        /// §15: T2 at 1M rows exceeds the soft budget **on purpose** — it
        /// exercises the pause mechanism. For the pure throughput measurement
        /// it must be disabled, otherwise you measure the pause and not the
        /// decoder.
        #[arg(long)]
        no_budget: bool,
    },
    /// Measures all three tables and prints the overall verdict.
    All {
        #[arg(long, env = "ALAMO_BENCH_URL")]
        url: String,
        #[arg(long, default_value_t = 5)]
        reps: usize,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Prepare {
            url,
            rows,
            table,
            force,
        } => prepare(&url, rows, &table, force).await,
        Command::Run {
            url,
            table,
            reps,
            no_budget,
        } => match Table::parse(&table) {
            Some(t) => run_one(&url, t, reps, no_budget).await.map(|r| r.print()),
            None => Err(format!("unknown table: {table} (expected t1, t2, t3)")),
        },
        Command::All { url, reps } => run_all(&url, reps).await,
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn connect(url: &str) -> Result<PgSession, String> {
    PgSession::connect_plain(url)
        .await
        .map_err(|e| format!("connection failed: {e}"))
}

// ---------------------------------------------------------------------------
// prepare
// ---------------------------------------------------------------------------

async fn prepare(url: &str, rows: u64, tables: &[String], force: bool) -> Result<(), String> {
    let selected: Vec<Table> = if tables.is_empty() {
        Table::all().to_vec()
    } else {
        tables
            .iter()
            .map(|t| Table::parse(t).ok_or_else(|| format!("unknown table: {t}")))
            .collect::<Result<_, _>>()?
    };

    let session = connect(url).await?;
    let client = &session.client;

    for t in selected {
        if force {
            client
                .batch_execute(&format!("DROP TABLE IF EXISTS {}", t.name()))
                .await
                .map_err(|e| format!("drop {}: {e}", t.name()))?;
        }
        client
            .batch_execute(&t.ddl())
            .await
            .map_err(|e| format!("create {}: {e} (use --force to recreate)", t.name()))?;

        // In batches: a single 1M-row INSERT works but gives no progress
        // signal at all, and on a slow machine it looks stuck.
        const BATCH: u64 = 100_000;
        let mut done = 0u64;
        let started = Instant::now();
        while done < rows {
            let from = done + 1;
            let to = (done + BATCH).min(rows);
            client
                .batch_execute(&t.fill_sql(from, to))
                .await
                .map_err(|e| format!("fill {}: {e}", t.name()))?;
            done = to;
            print!("\r  {} {done}/{rows}", t.name());
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
        println!(
            "\r  {} {rows} rows in {:.1}s",
            t.name(),
            started.elapsed().as_secs_f64()
        );

        // ANALYZE: without it, the first SELECT also pays for the missing
        // statistics and the first repetition turns out artificially slow.
        client
            .batch_execute(&format!("ANALYZE {}", t.name()))
            .await
            .map_err(|e| format!("analyze {}: {e}", t.name()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

struct Measure {
    rows: u64,
    wire_bytes: u64,
    layout_bytes: usize,
    first_row_ms: f64,
    total_ms: f64,
    ceiling_first_row_ms: f64,
    ceiling_total_ms: f64,
    window_p99_ms: f64,
    columns: usize,
    undecoded: Vec<String>,
    rss_steady: Option<u64>,
    rss_peak: Option<u64>,
}

async fn run_one(url: &str, table: Table, reps: usize, no_budget: bool) -> Result<Report, String> {
    let session = connect(url).await?;
    let client = &session.client;
    let sql = table.select_sql();

    let opts = FetchOptions {
        soft_budget_bytes: if no_budget {
            None
        } else {
            FetchOptions::default().soft_budget_bytes
        },
        ..Default::default()
    };

    // 1) The ceiling. It must be measured first and on the same connection:
    //    it is the denominator of everything else.
    let mut ceiling_first = Vec::with_capacity(reps);
    let mut ceiling_total = Vec::with_capacity(reps);
    for _ in 0..reps {
        let s = measure_ceiling(client, &sql)
            .await
            .map_err(|e| format!("ceiling: {e}"))?;
        ceiling_first.push(ms(s.time_to_first_row.unwrap_or_default()));
        ceiling_total.push(ms(s.total));
    }

    // 2) The full pipeline.
    let mut first = Vec::with_capacity(reps);
    let mut total = Vec::with_capacity(reps);
    let mut last = None;
    for _ in 0..reps {
        let (rs, s) = execute_collect(client, &sql, &opts)
            .await
            .map_err(|e| format!("execution: {e}"))?;
        first.push(ms(s.time_to_first_row.unwrap_or_default()));
        total.push(ms(s.total));
        last = Some((rs, s));
    }
    let (result_set, stats) = last.expect("at least one repetition");

    // 3) fetch_window: 100 rows x 20 columns, p99 over 1000 scattered
    //    extractions. Not sequential: a real scroll jumps around, and a
    //    contiguous window would measure the cache instead of the addressing.
    let cols = result_set.schema().len().min(20);
    let mut window_times = Vec::with_capacity(1000);
    let mut lcg: u64 = 0x2545_F491_4F6C_DD1D;
    let max_start = result_set.rows().saturating_sub(100);
    for _ in 0..1000 {
        lcg = lcg
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let start = if max_start == 0 {
            0
        } else {
            (lcg >> 33) % max_start
        };
        let t = Instant::now();
        let w = result_set.window(start..start + 100, 0..cols);
        window_times.push(ms(t.elapsed()));
        std::hint::black_box(&w);
    }

    let m = Measure {
        rows: stats.rows,
        wire_bytes: stats.wire_bytes,
        layout_bytes: result_set.byte_size(),
        first_row_ms: report::median(&mut first),
        total_ms: report::median(&mut total),
        ceiling_first_row_ms: report::median(&mut ceiling_first),
        ceiling_total_ms: report::median(&mut ceiling_total),
        window_p99_ms: report::percentile(&mut window_times, 99.0),
        columns: result_set.schema().len(),
        undecoded: stats.undecoded_columns.clone(),
        rss_steady: rss::current_rss(),
        rss_peak: rss::peak_rss(),
    };
    Ok(build_report(table, &m, reps, no_budget))
}

async fn run_all(url: &str, reps: usize) -> Result<(), String> {
    let mut worst = Band::Green;
    for t in Table::all() {
        // T2 exceeds the soft budget on purpose: for the throughput
        // measurement it must be disabled (§15).
        let r = run_one(url, t, reps, true).await?;
        r.print();
        worst = match (worst, r.worst_band()) {
            (Band::Red, _) | (_, Band::Red) => Band::Red,
            (Band::Yellow, _) | (_, Band::Yellow) => Band::Yellow,
            _ => Band::Green,
        };
    }
    println!("\n===============================================");
    println!("M0 PHASE 1 OUTCOME (CLI, without webview): {worst}");
    println!("{}", report::action_for(worst));
    println!(
        "\nNot measured here, and blocking for M0 (§15 phases 2-4):\n\
         \x20 · frame time on continuous scroll of 20k rows — the HEADLINE METRIC,\n\
         \x20   requires Tauri + Glide. \"zero frames above 50 ms\" weighs more than the average:\n\
         \x20   a single 200 ms stutter is noticed far more than a constant 20 ms;\n\
         \x20 · cold start;\n\
         \x20 · Windows and macOS as real gates;\n\
         \x20 · Linux/WebKitGTK smoke test (informative, not blocking — §1.2)."
    );
    Ok(())
}

fn build_report(table: Table, m: &Measure, reps: usize, no_budget: bool) -> Report {
    let rows_per_s = if m.total_ms > 0.0 {
        m.rows as f64 / (m.total_ms / 1000.0)
    } else {
        0.0
    };
    let ceiling_rows_per_s = if m.ceiling_total_ms > 0.0 {
        m.rows as f64 / (m.ceiling_total_ms / 1000.0)
    } else {
        0.0
    };
    let overhead_ms = (m.first_row_ms - m.ceiling_first_row_ms).max(0.0);
    let ram_ratio = if m.wire_bytes > 0 {
        m.layout_bytes as f64 / m.wire_bytes as f64
    } else {
        0.0
    };

    let (thr_green, thr_yellow) = match table {
        Table::Narrow => (1_000_000.0, 400_000.0),
        Table::Wide => (300_000.0, 120_000.0),
        // T3 has no threshold in the document: it serves to isolate the
        // per-column overhead, and is read relative to T1 — not in absolute.
        Table::Int60 => (0.0, 0.0),
    };

    let mut metrics = vec![
        Metric {
            name: "first row (end-to-end)",
            value: format!("{:.1} ms", m.first_row_ms),
            band: grade_lower_better(m.first_row_ms, 250.0, 600.0),
            thresholds: "<250 / 250-600 / >600".into(),
        },
        Metric {
            name: "pipeline overhead",
            value: format!("{overhead_ms:.1} ms"),
            band: grade_lower_better(overhead_ms, 30.0, 80.0),
            thresholds: "<30 / 30-80 / >80".into(),
        },
        Metric {
            name: "throughput",
            value: format!("{:.0} rows/s", rows_per_s),
            band: if thr_green > 0.0 {
                grade_higher_better(rows_per_s, thr_green, thr_yellow)
            } else {
                Band::NotApplicable
            },
            thresholds: if thr_green > 0.0 {
                format!(
                    ">={:.0}k / {:.0}k-{:.0}k / <{:.0}k",
                    thr_green / 1000.0,
                    thr_yellow / 1000.0,
                    thr_green / 1000.0,
                    thr_yellow / 1000.0
                )
            } else {
                "no threshold: read relative to T1".into()
            },
        },
        Metric {
            name: "RAM / logical size",
            value: format!("{ram_ratio:.2}x"),
            band: grade_lower_better(ram_ratio, 1.3, 1.8),
            thresholds: "<=1.3 / 1.3-1.8 / >1.8".into(),
        },
        Metric {
            name: "fetch_window p99 (100x20)",
            value: format!("{:.3} ms", m.window_p99_ms),
            band: grade_lower_better(m.window_p99_ms, 10.0, 25.0),
            thresholds: "<10 / 10-25 / >25".into(),
        },
    ];

    match (m.rss_peak, m.rss_steady) {
        (Some(peak), Some(steady)) if steady > 0 => {
            let ratio = peak as f64 / steady as f64;
            metrics.push(Metric {
                name: "peak RSS / steady RSS",
                value: format!("{ratio:.2}x"),
                band: if ratio <= 1.3 {
                    Band::Green
                } else if ratio < 2.0 {
                    Band::Yellow
                } else {
                    Band::Red
                },
                thresholds: "<=1.3 / … / >=2".into(),
            });
        }
        _ => metrics.push(Metric {
            name: "peak RSS / steady RSS",
            value: "not available".into(),
            band: Band::NotApplicable,
            thresholds: "see rss.rs".into(),
        }),
    }

    let bytes_per_row = m.wire_bytes.checked_div(m.rows).unwrap_or(0);
    let pct = if ceiling_rows_per_s > 0.0 {
        rows_per_s / ceiling_rows_per_s * 100.0
    } else {
        0.0
    };
    let mut notes = vec![
        format!(
            "{} rows x {} columns · wire {} · layout {} · {reps} repetitions, median",
            m.rows,
            m.columns,
            rss::format_bytes(m.wire_bytes),
            rss::format_bytes(m.layout_bytes as u64),
        ),
        // If the measured value deviates a lot from the nominal one, the
        // dataset is not the one in the document and the numbers cannot be
        // compared with any other run. Worth noticing here, not later.
        format!(
            "bytes/row on the wire: {bytes_per_row} (nominal in the document: {})",
            table.nominal_row_bytes()
        ),
        format!(
            "ceiling (read-and-discard): {ceiling_rows_per_s:.0} rows/s — the pipeline delivers {pct:.0}% of it"
        ),
    ];
    // Above 100% the "ceiling" is not a ceiling: it means its measurement
    // was taken with a colder cache (it runs first). It must be said,
    // otherwise it reads as an impossible result and trust in the whole
    // table is lost.
    if pct > 100.0 {
        notes.push(
            "⚠️ the pipeline exceeds the ceiling: the ceiling is measured first and pays for the \
             server's cold cache. Read it as 'decoding is not the bottleneck', not as an \
             absolute value."
                .into(),
        );
    }
    if no_budget {
        notes.push("memory soft budget DISABLED (pure throughput measurement)".into());
    }
    if !m.undecoded.is_empty() {
        notes.push(format!(
            "⚠️ columns with uninterpreted binary format: {} — see open decision 3 of §14",
            m.undecoded.join(", ")
        ));
    }

    Report {
        title: format!("{} — {}", table.label(), table.name()),
        metrics,
        notes,
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}
