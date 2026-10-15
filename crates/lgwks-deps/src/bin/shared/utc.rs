//! The current time as a UTC calendar stamp, without a time crate.

use std::time::{SystemTime, UNIX_EPOCH};

/// `YYYY-MM-DDTHH:MM:SSZ` for a count of seconds since the Unix epoch.
pub(crate) fn stamp_of(seconds: u64) -> String {
    let days = seconds.div_euclid(86_400);
    let within = seconds.rem_euclid(86_400);
    let shifted = days.saturating_add(719_468);
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era = day_of_era
        .saturating_sub(day_of_era.div_euclid(1_460))
        .saturating_add(day_of_era.div_euclid(36_524))
        .saturating_sub(day_of_era.div_euclid(146_096))
        .div_euclid(365);
    let day_of_year = day_of_era.saturating_sub(
        year_of_era
            .saturating_mul(365)
            .saturating_add(year_of_era.div_euclid(4))
            .saturating_sub(year_of_era.div_euclid(100)),
    );
    let month_slot = day_of_year
        .saturating_mul(5)
        .saturating_add(2)
        .div_euclid(153);
    let day = day_of_year
        .saturating_sub(
            month_slot
                .saturating_mul(153)
                .saturating_add(2)
                .div_euclid(5),
        )
        .saturating_add(1);
    let month = if month_slot < 10 {
        month_slot.saturating_add(3)
    } else {
        month_slot.saturating_sub(9)
    };
    let year = year_of_era.saturating_add(era.saturating_mul(400));
    let year = if month <= 2 {
        year.saturating_add(1)
    } else {
        year
    };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        within.div_euclid(3_600),
        within.rem_euclid(3_600).div_euclid(60),
        within.rem_euclid(60)
    )
}

/// The current time as a UTC stamp.
pub(crate) fn stamp() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => stamp_of(elapsed.as_secs()),
        Err(error) => {
            lgwks_std::trace::debug!(%error, "the clock reads before the epoch");
            stamp_of(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::stamp_of;

    #[test]
    fn a_unix_time_reads_as_its_utc_calendar_stamp() {
        assert_eq!(stamp_of(0), "1970-01-01T00:00:00Z", "the epoch");
        assert_eq!(stamp_of(951_782_400), "2000-02-29T00:00:00Z", "a leap day");
        assert_eq!(
            stamp_of(1_700_000_000),
            "2023-11-14T22:13:20Z",
            "an ordinary time"
        );
        assert_eq!(
            stamp_of(4_102_444_799),
            "2099-12-31T23:59:59Z",
            "the end of a century"
        );
    }
}
