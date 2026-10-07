use crate::{
    SAMPLE_RATE,
    manifest::{Track, Transition},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Options {
    pub overlap: f64,
    pub tail_window: f64,
    pub intro_window: f64,
    pub min_beat_score: f64,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            overlap: 1.0,
            tail_window: 12.0,
            intro_window: 12.0,
            min_beat_score: 0.5,
        }
    }
}
impl Options {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.overlap.is_finite() && (0.05..=10.0).contains(&self.overlap),
            "Overlap must be between 0.05 and 10 seconds"
        );
        ensure!(
            self.tail_window.is_finite() && self.tail_window >= 0.0 && self.tail_window <= 3600.0,
            "Tail search window must be 0..3600 seconds"
        );
        ensure!(
            self.intro_window.is_finite()
                && self.intro_window >= 0.0
                && self.intro_window <= 3600.0,
            "Intro search window must be 0..3600 seconds"
        );
        ensure!(
            self.min_beat_score.is_finite() && (0.0..=1.0).contains(&self.min_beat_score),
            "Minimum beat score must be 0..1"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Override {
    pub outgoing_beat_seconds: f64,
    pub incoming_beat_seconds: f64,
    pub overlap_seconds: Option<f64>,
}
pub type Overrides = BTreeMap<String, Override>;

pub fn seconds_to_samples(seconds: f64) -> Result<u64> {
    ensure!(
        seconds.is_finite() && seconds >= 0.0 && seconds < u64::MAX as f64 / SAMPLE_RATE as f64,
        "Invalid transition timestamp/duration: {seconds}"
    );
    Ok((seconds * SAMPLE_RATE as f64).round() as u64)
}

fn candidate_rank(tail: u64, intro: u64, outgoing: u64, incoming: u64) -> (u64, u64, u64, u64) {
    (tail + intro, tail, outgoing, incoming)
}

/// Rank by total discarded samples, then outgoing tail trim, then beat sample positions.
/// All ranking uses integers, so ties are independent of iteration/filesystem order.
pub fn select(
    out: &Track,
    incoming: &Track,
    options: &Options,
    manual: Option<&Override>,
    retained_start: u64,
    preceding_overlap: u64,
) -> Result<Transition> {
    options.validate()?;
    let overlap = manual
        .and_then(|v| v.overlap_seconds)
        .unwrap_or(options.overlap);
    ensure!(
        overlap.is_finite() && (0.05..=10.0).contains(&overlap),
        "Override overlap must be 0.05..10 seconds"
    );
    let n = seconds_to_samples(overlap)?;
    let pre = n / 2;
    let post = n - pre;
    let out_frames = out.frames.context("Outgoing duration unavailable")?;
    let in_frames = incoming.frames.context("Incoming duration unavailable")?;
    let min_fade_start = retained_start
        .checked_add(preceding_overlap)
        .context("Transition bounds overflow")?;
    let valid = |bo: u64, bi: u64| -> bool {
        bo >= pre
            && bi >= pre
            && bo.checked_add(post).is_some_and(|v| v <= out_frames)
            && bi.checked_add(post).is_some_and(|v| v <= in_frames)
            && bo - pre >= min_fade_start
    };
    let (bo, bi, source) = if let Some(manual) = manual {
        let bo = seconds_to_samples(manual.outgoing_beat_seconds)?;
        let bi = seconds_to_samples(manual.incoming_beat_seconds)?;
        ensure!(
            valid(bo, bi),
            "Override {}->{} does not leave room for the overlap, or collides with the preceding crossfade",
            out.index,
            incoming.index
        );
        (bo, bi, "user_override")
    } else {
        let out_analysis = out
            .analysis
            .as_ref()
            .context("Outgoing beat analysis missing")?;
        let in_analysis = incoming
            .analysis
            .as_ref()
            .context("Incoming beat analysis missing")?;
        let max_tail = seconds_to_samples(options.tail_window)?;
        let max_intro = seconds_to_samples(options.intro_window)?;
        type Candidate = ((u64, u64, u64, u64), u64, u64);
        let mut best: Option<Candidate> = None;
        for a in &out_analysis.beats {
            if !a.score.is_finite()
                || a.score < options.min_beat_score
                || a.sample < pre
                || !a.sample.checked_add(post).is_some_and(|v| v <= out_frames)
                || a.sample - pre < min_fade_start
                || out_frames - a.sample - post > max_tail
            {
                continue;
            }
            for b in &in_analysis.beats {
                if !b.score.is_finite()
                    || b.score < options.min_beat_score
                    || !valid(a.sample, b.sample)
                {
                    continue;
                }
                let tail = out_frames - a.sample - post;
                let intro = b.sample - pre;
                if tail > max_tail || intro > max_intro {
                    continue;
                }
                let rank = candidate_rank(tail, intro, a.sample, b.sample);
                if best.as_ref().is_none_or(|(old, _, _)| rank < *old) {
                    best = Some((rank, a.sample, b.sample));
                }
            }
        }
        let Some((_, bo, bi)) = best else {
            bail!(
                "No suitable detected beat pair for {}->{} ({:?} -> {:?}). Refusing an unaligned transition. Increase search windows, review --min-beat-score, or supply --overrides FILE with explicit timestamps",
                out.index,
                incoming.index,
                out.title,
                incoming.title
            );
        };
        (bo, bi, "detected_beats")
    };
    let mut warnings = vec![];
    if manual.is_some() {
        warnings.push("Explicit user override: timestamp alignment is accepted without requiring detected beats; search windows are bypassed".into());
    }
    if let (Some(a), Some(b)) = (
        out.analysis.as_ref().and_then(|a| a.bpm),
        incoming.analysis.as_ref().and_then(|a| a.bpm),
    ) && (a - b).abs() > 1.0
    {
        warnings.push(format!("Different estimated BPMs ({a:.2} vs {b:.2}): approximately {:.3} beats of drift over this overlap; no tempo correction is applied", (a - b).abs() * n as f64 / SAMPLE_RATE as f64 / 60.0));
    }
    Ok(Transition {
        outgoing_index: out.index,
        incoming_index: incoming.index,
        outgoing_beat_sample: bo,
        incoming_beat_sample: bi,
        outgoing_beat_seconds: bo as f64 / SAMPLE_RATE as f64,
        incoming_beat_seconds: bi as f64 / SAMPLE_RATE as f64,
        overlap_samples: n,
        overlap_seconds: n as f64 / SAMPLE_RATE as f64,
        outgoing_end_sample: bo + post,
        incoming_start_sample: bi - pre,
        tail_trim_seconds: (out_frames - bo - post) as f64 / SAMPLE_RATE as f64,
        intro_trim_seconds: (bi - pre) as f64 / SAMPLE_RATE as f64,
        source: source.into(),
        warnings,
    })
}

pub fn plan(tracks: &[Track], options: &Options, overrides: &Overrides) -> Result<Vec<Transition>> {
    options.validate()?;
    ensure!(!tracks.is_empty(), "Cannot plan an empty playlist");
    let keys: Vec<String> = tracks
        .windows(2)
        .map(|p| format!("{}->{}", p[0].index, p[1].index))
        .collect();
    for key in overrides.keys() {
        ensure!(
            keys.contains(key),
            "Override key {key:?} does not name an adjacent playlist pair"
        );
    }
    let mut transitions: Vec<Transition> = vec![];
    for (pair, key) in tracks.windows(2).zip(keys) {
        let (start, previous_n) = transitions
            .last()
            .map(|t| (t.incoming_start_sample, t.overlap_samples))
            .unwrap_or((0, 0));
        transitions.push(select(
            &pair[0],
            &pair[1],
            options,
            overrides.get(&key),
            start,
            previous_n,
        )?);
    }
    Ok(transitions)
}

#[cfg(test)]
mod tests {
    use super::candidate_rank;
    #[test]
    fn equal_cost_candidates_have_explicit_total_order() {
        assert!(candidate_rank(20, 80, 900, 100) < candidate_rank(40, 60, 850, 80));
        assert!(candidate_rank(20, 80, 900, 100) < candidate_rank(20, 80, 901, 100));
        assert!(candidate_rank(20, 80, 900, 100) < candidate_rank(20, 80, 900, 101));
    }
}
