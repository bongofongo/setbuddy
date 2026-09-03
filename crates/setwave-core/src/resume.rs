//! When a saved position is worth keeping, and worth using.
//!
//! Resume is the feature that makes a two-hour set behave like an album you can
//! walk away from. The thresholds exist so it never gets in the way: nobody
//! wants "resume at 0:04", and resuming four seconds before the end just replays
//! the applause.

/// Below this, the listener has barely started; treat it as not worth resuming.
pub const MIN_RESUME_SECS: f64 = 30.0;

/// Within this much of the end, the track is effectively finished.
pub const END_MARGIN_SECS: f64 = 60.0;

/// Whether `position` in a file of `duration` should be persisted.
///
/// With an unknown duration — a file we could not probe — only the head rule
/// applies, since there is no end to be near.
pub fn should_store(position_secs: f64, duration_secs: Option<f64>) -> bool {
    if !position_secs.is_finite() || position_secs < MIN_RESUME_SECS {
        return false;
    }
    match duration_secs {
        Some(duration) if duration.is_finite() && duration > 0.0 => {
            duration - position_secs >= END_MARGIN_SECS
        }
        _ => true,
    }
}

/// The offset to start at, given a stored position. `None` means start over.
///
/// Applies the same rules as [`should_store`], so a position that became stale —
/// the file was replaced by a shorter one, say — quietly starts from the top
/// instead of seeking past the end.
pub fn start_at(stored_secs: Option<f64>, duration_secs: Option<f64>) -> Option<f64> {
    let stored = stored_secs?;
    should_store(stored, duration_secs).then_some(stored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_the_first_thirty_seconds() {
        assert!(!should_store(0.0, Some(7200.0)));
        assert!(!should_store(29.9, Some(7200.0)));
        assert!(should_store(30.0, Some(7200.0)));
    }

    #[test]
    fn ignores_the_last_minute() {
        assert!(should_store(7139.0, Some(7200.0)));
        assert!(!should_store(7141.0, Some(7200.0)));
        assert!(!should_store(7200.0, Some(7200.0)));
    }

    #[test]
    fn short_files_never_resume() {
        // A 45s interlude is inside both guards at once; that is intended.
        assert!(!should_store(35.0, Some(45.0)));
    }

    #[test]
    fn unknown_duration_applies_only_the_head_rule() {
        assert!(!should_store(10.0, None));
        assert!(should_store(600.0, None));
    }

    #[test]
    fn rejects_nonsense_positions() {
        assert!(!should_store(f64::NAN, Some(100.0)));
        assert!(!should_store(f64::INFINITY, Some(7200.0)));
        assert!(!should_store(-5.0, Some(7200.0)));
    }

    #[test]
    fn start_at_declines_stale_positions() {
        assert_eq!(start_at(Some(3600.0), Some(7200.0)), Some(3600.0));
        assert_eq!(start_at(None, Some(7200.0)), None);
        // File was replaced by a much shorter one: don't seek past the end.
        assert_eq!(start_at(Some(3600.0), Some(120.0)), None);
    }
}
