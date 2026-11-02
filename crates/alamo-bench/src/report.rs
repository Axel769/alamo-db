//! Thresholds and report rendering — §15.
//!
//! ⚠️ **Thresholds must be fixed before running the spike**, otherwise you
//! look at the results and rationalise them. This file *is* that
//! pre-commitment: if a number fails, the reaction is already written.
//!
//! A binary pass/fail is not enough: you need to know **what to do**.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    Green,
    Yellow,
    Red,
    /// Not measurable in this phase (frame time and cold start require
    /// Tauri + Glide: §15 phase 2).
    NotApplicable,
}

impl fmt::Display for Band {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Band::Green => "GREEN",
            Band::Yellow => "YELLOW",
            Band::Red => "RED",
            Band::NotApplicable => "n/a",
        };
        f.write_str(s)
    }
}

/// "Lower is better" threshold.
pub fn grade_lower_better(value: f64, green_below: f64, yellow_below: f64) -> Band {
    if value < green_below {
        Band::Green
    } else if value < yellow_below {
        Band::Yellow
    } else {
        Band::Red
    }
}

/// "Higher is better" threshold.
pub fn grade_higher_better(value: f64, green_at_or_above: f64, yellow_at_or_above: f64) -> Band {
    if value >= green_at_or_above {
        Band::Green
    } else if value >= yellow_at_or_above {
        Band::Yellow
    } else {
        Band::Red
    }
}

#[derive(Debug, Clone)]
pub struct Metric {
    pub name: &'static str,
    pub value: String,
    pub band: Band,
    /// Thresholds spelled out in the report: whoever reads it six months
    /// from now must not have to look them up in the document.
    pub thresholds: String,
}

pub struct Report {
    pub title: String,
    pub metrics: Vec<Metric>,
    pub notes: Vec<String>,
}

impl Report {
    pub fn worst_band(&self) -> Band {
        if self.metrics.iter().any(|m| m.band == Band::Red) {
            Band::Red
        } else if self.metrics.iter().any(|m| m.band == Band::Yellow) {
            Band::Yellow
        } else {
            Band::Green
        }
    }

    pub fn print(&self) {
        println!("\n=== {} ===", self.title);
        let w = self
            .metrics
            .iter()
            .map(|m| m.name.len())
            .max()
            .unwrap_or(20)
            .max(20);
        for m in &self.metrics {
            println!(
                "  {:<w$}  {:>16}  {:<7} [{}]",
                m.name,
                m.value,
                m.band.to_string(),
                m.thresholds,
                w = w
            );
        }
        for n in &self.notes {
            println!("  · {n}");
        }
        println!("  -> overall outcome: {}", self.worst_band());
        println!("{}", action_for(self.worst_band()));
    }
}

/// §15, "what to do per band". The line that matters is the red one on
/// frame time: it is not a bug to profile, it is an **architectural
/// decision**.
pub fn action_for(band: Band) -> &'static str {
    match band {
        Band::Green => "  Action: none. -> M1.",
        Band::Yellow => {
            "  Action: proceed, but the item enters the optimisation backlog with the recorded measurement."
        }
        Band::Red => {
            "  Action: if the red is on throughput or memory, PROFILE before concluding — it is\n\
           \x20         almost always a hidden per-cell allocation, not a limit of the stack.\n\
           \x20         If the red is on frame time, it is an architectural decision:\n\
           \x20         plan B of §1.2 (controlled degradation / virtualised DOM grid / stack change)."
        }
        Band::NotApplicable => "",
    }
}

/// Median of N repetitions. §15: 5 repetitions, **median** — the mean gets
/// polluted by the first cold-cache run.
pub fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// Nearest-rank percentile. Used for `fetch_window`, where §15 asks for the
/// p99 and not the mean: it is the tail you see while scrolling.
pub fn percentile(values: &mut [f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((p / 100.0) * values.len() as f64).ceil() as usize;
    values[rank.saturating_sub(1).min(values.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_of_five() {
        let mut v = vec![5.0, 1.0, 3.0, 2.0, 4.0];
        assert_eq!(median(&mut v), 3.0);
    }

    #[test]
    fn p99_picks_the_tail_not_the_average() {
        let mut v: Vec<f64> = (0..100).map(|i| i as f64).collect();
        assert_eq!(percentile(&mut v, 99.0), 98.0);
    }

    #[test]
    fn bands_match_the_document() {
        // First row on screen: <250 green, 250-600 yellow, >600 red.
        assert_eq!(grade_lower_better(120.0, 250.0, 600.0), Band::Green);
        assert_eq!(grade_lower_better(400.0, 250.0, 600.0), Band::Yellow);
        assert_eq!(grade_lower_better(900.0, 250.0, 600.0), Band::Red);
        // T1 throughput: >=1M green, 400k-1M yellow, <400k red.
        assert_eq!(grade_higher_better(1_200_000.0, 1e6, 4e5), Band::Green);
        assert_eq!(grade_higher_better(500_000.0, 1e6, 4e5), Band::Yellow);
        assert_eq!(grade_higher_better(300_000.0, 1e6, 4e5), Band::Red);
    }
}
