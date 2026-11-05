//! What the running binary is: its crate version and the instant it was compiled.
//!
//! [`BUILD_EPOCH`] is stamped by `build.rs` from the compile clock, so it reports
//! when this crate was last compiled. A rebuild that only relinks an unchanged
//! crate keeps the previous stamp; that is the intended reading of "when was this
//! built", not a bug.

/// The crate version from `Cargo.toml` (`0.1.3`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Unix seconds of the compile, written by `build.rs`.
pub const BUILD_EPOCH: &str = env!("GHOSTWAVE_BUILD_EPOCH");

/// Formats a unix timestamp as `YYYY-MM-DD HH:MM UTC` (civil-from-days, no dependency).
pub fn format_utc(epoch_secs: u64) -> String {
    let days = (epoch_secs / 86_400) as i64;
    let secs_of_day = epoch_secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60
    )
}

/// When this crate was compiled, `YYYY-MM-DD HH:MM UTC`.
///
/// # Panics
///
/// Panics when [`BUILD_EPOCH`] is not a unix timestamp, which can only happen if
/// the crate is compiled without `build.rs` running. Failing loudly is deliberate:
/// a silent `1970-01-01` would claim the wrong build date.
pub fn build_date() -> String {
    format_utc(
        BUILD_EPOCH
            .parse()
            .expect("build.rs sets GHOSTWAVE_BUILD_EPOCH to unix seconds"),
    )
}

/// `ghostwave 0.1.3 — built 2026-10-30 05:12 UTC` — the one line both surfaces show.
pub fn summary() -> String {
    format!("ghostwave {VERSION} — built {}", build_date())
}

/// Days since 1970-01-01 to a proleptic-Gregorian `(year, month, day)` (Howard
/// Hinnant's civil-from-days). Valid for every date the compile clock can produce.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (year + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_the_epoch() {
        assert_eq!(format_utc(0), "1970-01-01 00:00 UTC");
    }

    #[test]
    fn formats_a_known_modern_instant() {
        assert_eq!(format_utc(1_700_000_000), "2023-11-14 22:13 UTC");
    }

    #[test]
    fn formats_a_leap_day() {
        assert_eq!(format_utc(951_782_400), "2000-02-29 00:00 UTC");
    }

    #[test]
    fn the_build_stamp_is_wired() {
        let epoch: u64 = BUILD_EPOCH
            .parse()
            .expect("build.rs sets GHOSTWAVE_BUILD_EPOCH to unix seconds");
        assert!(
            epoch > 1_600_000_000,
            "build stamp {epoch} looks like a missing build.rs stamp"
        );
    }

    #[test]
    fn summary_names_the_version_and_the_date() {
        let line = summary();
        assert!(line.starts_with("ghostwave "), "got {line:?}");
        assert!(line.contains(env!("CARGO_PKG_VERSION")), "got {line:?}");
        assert!(line.contains(" — built "), "got {line:?}");
        assert!(line.ends_with(" UTC"), "got {line:?}");
    }
}
