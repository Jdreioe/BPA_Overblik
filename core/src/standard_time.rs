//! Optional shift hours shared by TeamUp all-day events and undated sheet times.
//!
//! The app calls these »Vagter uden tid«: the setting is named after the
//! shifts it applies to, because Danish »standardtid« is winter time.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::sheets::{interval, split_range};

const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// The Danish weekday of `date`, lower case, for messages such as »om
/// mandagen«.
pub fn weekday_da(date: NaiveDate) -> &'static str {
    [
        "mandag", "tirsdag", "onsdag", "torsdag", "fredag", "lørdag", "søndag",
    ][date.weekday().num_days_from_monday() as usize]
}

/// Why a shift without a time has none: no time is chosen for its weekday.
pub fn missing_message(shift: &str, date: NaiveDate) -> String {
    format!(
        "{shift} har ingen tid, og der er ikke valgt en tid for vagter uden tid om {}en. Vælg en under Indstillinger → Vagter uden tid.",
        weekday_da(date)
    )
}

/// Why a shift without a time cannot take the chosen one: the clock changes
/// that night, so the time is missing or there twice.
pub fn clock_change_message(shift: &str) -> String {
    format!(
        "{shift} har ingen tid, og tiden for vagter uden tid kan ikke bruges den dag, fordi uret stilles om. Giv vagten egne tider."
    )
}

pub type StandardInterval = (DateTime<FixedOffset>, DateTime<FixedOffset>);

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StandardTimes {
    /// Empty means no default. A weekday entry with `null` clears this default.
    #[serde(default)]
    pub everyday: String,
    #[serde(default)]
    pub weekdays: BTreeMap<String, Option<String>>,
}

impl StandardTimes {
    pub fn validate(&self) -> Result<(), &'static str> {
        let valid = |value: &str| {
            let (start, end) = split_range(value);
            !start.is_empty()
                && !end.is_empty()
                && interval(
                    NaiveDate::from_ymd_opt(2026, 2, 2).unwrap(),
                    start,
                    end,
                    chrono_tz::Europe::Copenhagen,
                )
                .is_some()
        };
        if !self.everyday.is_empty() && !valid(&self.everyday) {
            return Err("Tiden for vagter uden tid kan ikke læses. Vælg den igen.");
        }
        for (day, value) in &self.weekdays {
            if !DAYS.contains(&day.as_str()) || value.as_deref().is_some_and(|v| !valid(v)) {
                return Err("En ugedags tid for vagter uden tid kan ikke læses. Vælg den igen.");
            }
        }
        Ok(())
    }

    /// `None` means this weekday has no standard; an error is a DST clock time
    /// that cannot be interpreted safely on this particular date.
    pub fn on(&self, date: NaiveDate, zone: Tz) -> Result<Option<StandardInterval>, &'static str> {
        let day = DAYS[date.weekday().num_days_from_monday() as usize];
        let value = match self.weekdays.get(day) {
            Some(Some(value)) => value.as_str(),
            Some(None) => return Ok(None),
            None => &self.everyday,
        };
        if value.is_empty() {
            return Ok(None);
        }
        let (start, end) = split_range(value);
        interval(date, start, end, zone)
            .map(Some)
            .ok_or("Tiden for vagter uden tid kan ikke bruges den dag, fordi uret stilles om.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekday_can_override_or_clear_the_everyday_time() {
        let standard = StandardTimes {
            everyday: "6-22".into(),
            weekdays: BTreeMap::from([("sat".into(), Some("22-8".into())), ("sun".into(), None)]),
        };
        standard.validate().unwrap();
        let date = |day| NaiveDate::from_ymd_opt(2026, 9, day).unwrap();
        let zone = chrono_tz::Europe::Copenhagen;
        assert_eq!(
            standard
                .on(date(21), zone)
                .unwrap()
                .unwrap()
                .0
                .format("%H:%M")
                .to_string(),
            "06:00"
        );
        let saturday = standard.on(date(26), zone).unwrap().unwrap();
        assert_eq!(saturday.1.date_naive(), date(27));
        assert_eq!(saturday.1.format("%H:%M").to_string(), "08:00");
        assert!(standard.on(date(27), zone).unwrap().is_none());
    }

    #[test]
    fn ambiguous_dst_clock_time_is_rejected_for_its_date() {
        let standard = StandardTimes {
            everyday: "2-8".into(),
            ..Default::default()
        };
        standard.validate().unwrap();
        assert!(standard
            .on(
                NaiveDate::from_ymd_opt(2026, 10, 25).unwrap(),
                chrono_tz::Europe::Copenhagen
            )
            .is_err());
    }
}
