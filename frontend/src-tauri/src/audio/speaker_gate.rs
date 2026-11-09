//! "Me vs Them" speaker attribution helpers.
//!
//! The system-audio stream is a clean digital copy of the call, so any speech in it is
//! "Them". The microphone stream is "Me" plus an acoustic echo of "Them" whenever the
//! user plays the call through laptop speakers. [`EchoGate`] drops mic speech that
//! overlaps Them's speech (plus a short tail while the echo dies down), and
//! [`ThemDedup`] is a post-transcription safety net that drops a Me line that mostly
//! repeats a recent Them line.
//!
//! All timestamps handed to [`EchoGate`] are VAD-timeline milliseconds. Both VAD
//! instances are fed identical sample counts per window, so their timelines align.

use std::collections::VecDeque;

/// How long after Them stops speaking mic speech is still treated as echo.
pub const ECHO_TAIL_MS: f64 = 300.0;

/// System-audio VAD segments quieter than this RMS (~-60 dBFS) are treated as
/// noise/bleed, not as Them speaking, so they neither get transcribed nor gate the mic.
pub const SYSTEM_SPEECH_MIN_RMS: f32 = 0.001;

/// A Me line is dropped if it is within this many seconds of a Them line...
pub const DEDUP_WINDOW_SECS: f64 = 2.0;
/// ...and at least this fraction of its words also appear in that Them line.
pub const DEDUP_MIN_WORD_OVERLAP: f32 = 0.6;
/// Lines shorter than this many words are never dedup-dropped (too ambiguous).
pub const DEDUP_MIN_WORDS: usize = 3;

/// Only the last this-many ms of Them speech intervals are kept for gating.
const GATE_RETENTION_MS: f64 = 60_000.0;
/// Only this many recent Them lines are kept for dedup.
const DEDUP_MAX_ENTRIES: usize = 16;

/// Root-mean-square level of a sample buffer.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// Tracks where Them is speaking so overlapping mic speech can be dropped as echo.
#[derive(Debug, Default)]
pub struct EchoGate {
    /// Closed Them speech intervals `(start_ms, end_ms)`.
    them: VecDeque<(f64, f64)>,
    /// Start of Them speech that is still in progress (VAD segment not closed yet).
    open_since_ms: Option<f64>,
}

impl EchoGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a closed Them speech segment.
    pub fn record_them(&mut self, start_ms: f64, end_ms: f64) {
        self.them.push_back((start_ms, end_ms));
        while let Some(&(_, end)) = self.them.front() {
            if end + GATE_RETENTION_MS < end_ms {
                self.them.pop_front();
            } else {
                break;
            }
        }
    }

    /// Set (or clear) the start of Them speech currently in progress.
    pub fn set_open(&mut self, open_since_ms: Option<f64>) {
        self.open_since_ms = open_since_ms;
    }

    /// True if mic speech `[start_ms, end_ms]` overlaps Them speech (plus the echo tail).
    pub fn should_drop_mic(&self, start_ms: f64, end_ms: f64) -> bool {
        if let Some(open) = self.open_since_ms {
            if end_ms > open {
                return true;
            }
        }
        self.them
            .iter()
            .any(|&(t_start, t_end)| start_ms < t_end + ECHO_TAIL_MS && end_ms > t_start)
    }
}

fn normalize_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric() || *c == '\'')
                .flat_map(|c| c.to_lowercase())
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Remembers recent Them lines so a near-duplicate Me line can be dropped.
#[derive(Debug, Default)]
pub struct ThemDedup {
    /// `(start_secs, end_secs, normalized words)`
    recent: VecDeque<(f64, f64, Vec<String>)>,
}

impl ThemDedup {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_them(&mut self, start_secs: f64, end_secs: f64, text: &str) {
        self.recent
            .push_back((start_secs, end_secs, normalize_words(text)));
        while self.recent.len() > DEDUP_MAX_ENTRIES {
            self.recent.pop_front();
        }
    }

    /// True if this Me line mostly duplicates a Them line within [`DEDUP_WINDOW_SECS`].
    pub fn is_duplicate_of_them(&self, start_secs: f64, end_secs: f64, text: &str) -> bool {
        let me_words = normalize_words(text);
        if me_words.len() < DEDUP_MIN_WORDS {
            return false;
        }
        self.recent.iter().any(|(t_start, t_end, them_words)| {
            let near = start_secs < t_end + DEDUP_WINDOW_SECS && end_secs > t_start - DEDUP_WINDOW_SECS;
            if !near {
                return false;
            }
            // Multiset overlap: each Them word can match at most one Me word.
            let mut pool = them_words.clone();
            let matched = me_words
                .iter()
                .filter(|w| match pool.iter().position(|p| p == *w) {
                    Some(i) => {
                        pool.swap_remove(i);
                        true
                    }
                    None => false,
                })
                .count();
            matched as f32 / me_words.len() as f32 >= DEDUP_MIN_WORD_OVERLAP
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_passes_mic_when_them_silent() {
        let mut g = EchoGate::new();
        g.record_them(0.0, 1000.0);
        assert!(!g.should_drop_mic(2000.0, 3000.0));
    }

    #[test]
    fn gate_drops_overlapping_mic() {
        let mut g = EchoGate::new();
        g.record_them(1000.0, 3000.0);
        assert!(g.should_drop_mic(2000.0, 2500.0));
        assert!(g.should_drop_mic(500.0, 1500.0));
    }

    #[test]
    fn gate_drops_mic_inside_echo_tail_only() {
        let mut g = EchoGate::new();
        g.record_them(0.0, 1000.0);
        assert!(g.should_drop_mic(1000.0 + ECHO_TAIL_MS - 50.0, 2500.0));
        assert!(!g.should_drop_mic(1000.0 + ECHO_TAIL_MS + 50.0, 2500.0));
    }

    #[test]
    fn gate_drops_mic_during_open_them_speech() {
        let mut g = EchoGate::new();
        g.set_open(Some(5000.0));
        assert!(g.should_drop_mic(5200.0, 6000.0));
        assert!(!g.should_drop_mic(1000.0, 2000.0));
        g.set_open(None);
        assert!(!g.should_drop_mic(5200.0, 6000.0));
    }

    #[test]
    fn dedup_drops_echoed_line() {
        let mut d = ThemDedup::new();
        d.record_them(10.0, 13.0, "So the quarterly numbers look strong this year");
        assert!(d.is_duplicate_of_them(11.0, 13.5, "the quarterly numbers look strong"));
    }

    #[test]
    fn dedup_keeps_distinct_or_far_or_short_lines() {
        let mut d = ThemDedup::new();
        d.record_them(10.0, 13.0, "So the quarterly numbers look strong this year");
        assert!(!d.is_duplicate_of_them(11.0, 12.0, "I disagree with that completely"));
        assert!(!d.is_duplicate_of_them(30.0, 33.0, "the quarterly numbers look strong"));
        assert!(!d.is_duplicate_of_them(11.0, 12.0, "numbers look"));
    }

    #[test]
    fn dedup_ignores_punctuation_and_case() {
        let mut d = ThemDedup::new();
        d.record_them(0.0, 2.0, "Hello, how are you today?");
        assert!(d.is_duplicate_of_them(0.5, 2.0, "hello how are you today"));
    }

    #[test]
    fn rms_of_silence_and_signal() {
        assert_eq!(rms(&[]), 0.0);
        assert_eq!(rms(&[0.0; 10]), 0.0);
        assert!((rms(&[0.5, -0.5]) - 0.5).abs() < 1e-6);
    }
}
