//! The rolling 24 h spend cap, ported line for line from Pactora on NEAR
//! (itself from v3 `AgentAccountCore`). Pure functions over `Purse`, so they
//! are tested without a chain.

use anchor_lang::prelude::*;

use crate::errors::FidoraError;
use crate::state::{Purse, BUCKETS, HOUR};

pub fn hour_of(now_s: u64) -> u64 {
    now_s / HOUR
}

/// The hour the window is at: never earlier than the last one written. The
/// Clock sysvar is an estimate and can step back; a spend then lands in the
/// newest bucket, so the window never reopens and nothing underflows.
fn effective_hour(p: &Purse, now_s: u64) -> u64 {
    hour_of(now_s).max(p.last_hour)
}

/// Drop the buckets that left the window since `last_hour`.
pub fn roll(p: &mut Purse, hour: u64) {
    let last = p.last_hour;
    if hour == last {
        return;
    }
    if hour - last >= BUCKETS {
        p.buckets.iter_mut().for_each(|b| *b = 0);
        p.window_total = 0;
    } else {
        let mut total = p.window_total;
        for h in (last + 1)..=hour {
            let i = (h % BUCKETS) as usize;
            let b = p.buckets[i];
            if b != 0 {
                total -= b;
                p.buckets[i] = 0;
            }
        }
        p.window_total = total;
    }
    p.last_hour = hour;
}

/// Check the caps and record `spent`.
pub fn charge(p: &mut Purse, spent: u64, now_s: u64) -> Result<()> {
    require!(spent <= p.per_task_cap, FidoraError::PerTaskCapExceeded);
    let hour = effective_hour(p, now_s);
    roll(p, hour);
    let remaining = p.daily_cap.saturating_sub(p.window_total);
    require!(spent <= remaining, FidoraError::DailyCapExceeded);
    p.window_total += spent;
    p.buckets[(hour % BUCKETS) as usize] += spent;
    Ok(())
}

/// The window's spend as `roll` would leave it, without writing.
pub fn window_spent(p: &Purse, now_s: u64) -> u64 {
    let hour = effective_hour(p, now_s);
    let last = p.last_hour;
    if hour - last >= BUCKETS {
        return 0;
    }
    let mut total = p.window_total;
    for h in (last + 1)..=hour {
        total -= p.buckets[(h % BUCKETS) as usize];
    }
    total
}

pub fn daily_remaining(p: &Purse, now_s: u64) -> u64 {
    p.daily_cap.saturating_sub(window_spent(p, now_s))
}

/// When the oldest counted spend leaves the window (seconds); 0 if none.
pub fn next_release_at(p: &Purse, now_s: u64) -> u64 {
    let hour = effective_hour(p, now_s);
    let last = p.last_hour;
    if hour - last >= BUCKETS {
        return 0;
    }
    let first = hour.saturating_sub(BUCKETS - 1);
    for h in first..=last {
        if p.buckets[(h % BUCKETS) as usize] != 0 {
            return (h + BUCKETS) * HOUR;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn purse(per_task: u64, daily: u64) -> Purse {
        Purse::new(per_task, daily)
    }

    #[test]
    fn per_task_cap_is_checked_before_the_window() {
        let mut p = purse(10, 100);
        assert!(charge(&mut p, 11, 0).is_err());
    }

    #[test]
    fn daily_cap_counts_spends_inside_the_window() {
        let mut p = purse(60, 100);
        charge(&mut p, 60, 0).unwrap();
        assert_eq!(daily_remaining(&p, 10 * HOUR), 40);
        let mut q = p.clone();
        assert!(charge(&mut q, 41, 10 * HOUR).is_err());
        charge(&mut p, 40, 10 * HOUR).unwrap();
        assert_eq!(daily_remaining(&p, 10 * HOUR), 0);
    }

    /// The v3 bug (2× the cap across a fixed day boundary): a rolling window
    /// refuses the second full spend one hour later.
    #[test]
    fn no_double_spend_across_a_day_boundary() {
        let mut p = purse(100, 100);
        let day = 24 * HOUR;
        charge(&mut p, 100, day - 1).unwrap();
        assert!(charge(&mut p.clone(), 1, day + 1).is_err());
    }

    #[test]
    fn a_spend_leaves_the_window_after_24_to_25_hours() {
        let mut p = purse(100, 100);
        let t0 = 30 * HOUR + 1_800;
        charge(&mut p, 100, t0).unwrap();
        assert_eq!(window_spent(&p, 54 * HOUR), 100);
        assert_eq!(window_spent(&p, 55 * HOUR), 0);
        assert_eq!(next_release_at(&p, t0), 55 * HOUR);
    }

    #[test]
    fn long_idle_clears_everything() {
        let mut p = purse(100, 100);
        charge(&mut p, 50, 0).unwrap();
        roll(&mut p, 1_000);
        assert_eq!(p.window_total, 0);
        assert!(p.buckets.iter().all(|b| *b == 0));
        assert_eq!(next_release_at(&p, 1_000 * HOUR), 0);
    }

    /// Solana's Clock is a stake-weighted estimate, not guaranteed monotonic.
    /// A step back across an hour boundary must neither panic (every spend
    /// would be refused until the clock caught up) nor reopen the window.
    #[test]
    fn a_clock_step_back_neither_panics_nor_reopens_the_window() {
        let mut p = purse(100, 100);
        charge(&mut p, 80, 10 * HOUR).unwrap();
        assert_eq!(window_spent(&p, 9 * HOUR + 3_599), 80);
        assert_eq!(daily_remaining(&p, 9 * HOUR), 20);
        assert_eq!(next_release_at(&p, 9 * HOUR), 35 * HOUR);
        assert!(charge(&mut p.clone(), 21, 9 * HOUR).is_err());
        charge(&mut p, 20, 9 * HOUR).unwrap();
        assert_eq!(p.window_total, 100);
        assert_eq!(p.last_hour, 10);
    }

    #[test]
    fn view_matches_what_roll_writes() {
        let mut p = purse(100, 1_000);
        for (i, t) in [0u64, 3, 7, 20, 24, 26, 40].iter().enumerate() {
            charge(&mut p, 10 + i as u64, t * HOUR).unwrap();
        }
        for h in 40..80u64 {
            let seen = window_spent(&p, h * HOUR);
            let mut q = p.clone();
            roll(&mut q, h);
            assert_eq!(seen, q.window_total, "hour {h}");
        }
    }
}
