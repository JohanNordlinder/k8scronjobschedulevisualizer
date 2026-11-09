//! Parsing and expanding CronJob schedules the way the Kubernetes CronJob
//! controller interprets them (5-field Vixie cron, `@` macros, optional time zone).

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use croner::parser::{CronParser, Seconds, Year};
use croner::{Cron, Direction};

#[derive(Clone, Debug)]
pub enum Schedule {
    Cron { cron: Cron, tz: Tz },
    /// `@every <duration>` (accepted by the robfig/cron parser Kubernetes uses).
    Every(Duration),
}

/// Parse `spec.schedule` together with `spec.timeZone`.
///
/// Without a time zone Kubernetes uses the kube-controller-manager's local zone,
/// which is UTC on OpenShift, so that is what we assume.
pub fn parse(schedule: &str, time_zone: Option<&str>) -> Result<Schedule, String> {
    let mut s = schedule.trim();
    let mut tz_name: Option<String> = time_zone.map(str::to_owned);

    // Legacy inline zone prefix: "CRON_TZ=Europe/Stockholm 0 3 * * *".
    for prefix in ["CRON_TZ=", "TZ="] {
        if let Some(rest) = s.strip_prefix(prefix) {
            let (tz, rest) = rest
                .split_once(char::is_whitespace)
                .ok_or_else(|| "time zone prefix without schedule".to_string())?;
            tz_name.get_or_insert_with(|| tz.to_string());
            s = rest.trim();
        }
    }

    let tz: Tz = match tz_name.as_deref() {
        Some(name) => name
            .parse()
            .map_err(|_| format!("unknown time zone '{name}'"))?,
        None => chrono_tz::UTC,
    };

    if let Some(rest) = s.strip_prefix("@every") {
        return parse_go_duration(rest.trim()).map(Schedule::Every);
    }
    let s = if s.eq_ignore_ascii_case("@midnight") {
        "0 0 * * *"
    } else {
        s
    };

    let cron = CronParser::builder()
        .seconds(Seconds::Disallowed)
        .year(Year::Disallowed)
        .build()
        .parse(s)
        .map_err(|e| e.to_string())?;
    Ok(Schedule::Cron { cron, tz })
}

impl Schedule {
    /// All trigger times in `[from, to)`, at most `cap` of them.
    pub fn occurrences(&self, from: DateTime<Utc>, to: DateTime<Utc>, cap: usize) -> Vec<DateTime<Utc>> {
        match self {
            Schedule::Cron { cron, tz } => cron
                .iter_from(from.with_timezone(tz), Direction::Forward)
                .map(|t| t.with_timezone(&Utc))
                .take_while(|t| *t < to)
                .take(cap)
                .collect(),
            Schedule::Every(step) => {
                let mut out = Vec::new();
                let mut t = from;
                while t < to && out.len() < cap {
                    out.push(t);
                    t += *step;
                }
                out
            }
        }
    }
}

/// Parse Go-style durations such as `90s`, `5m`, `1h30m`.
fn parse_go_duration(s: &str) -> Result<Duration, String> {
    let err = || format!("invalid @every duration '{s}'");
    let mut total = 0i64;
    let mut num = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            let n: i64 = num.parse().map_err(|_| err())?;
            num.clear();
            total += n * match c {
                'h' => 3600,
                'm' => 60,
                's' => 1,
                _ => return Err(err()),
            };
        }
    }
    if !num.is_empty() || total <= 0 {
        return Err(err());
    }
    Ok(Duration::seconds(total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone, Timelike};

    fn day() -> (DateTime<Utc>, DateTime<Utc>) {
        let from = Utc.with_ymd_and_hms(2026, 3, 2, 0, 0, 0).unwrap();
        (from, from + Duration::days(1))
    }

    fn count(sched: &str, tz: Option<&str>) -> usize {
        let (a, b) = day();
        parse(sched, tz).unwrap().occurrences(a, b, 100_000).len()
    }

    #[test]
    fn standard_patterns() {
        assert_eq!(count("*/5 * * * *", None), 288);
        assert_eq!(count("0 * * * *", None), 24);
        assert_eq!(count("@hourly", None), 24);
        assert_eq!(count("@daily", None), 1);
        assert_eq!(count("@midnight", None), 1);
        assert_eq!(count("@every 1h30m", None), 16);
    }

    #[test]
    fn time_zone_shifts_occurrence() {
        let (a, b) = day();
        let s = parse("0 3 * * *", Some("Europe/Stockholm")).unwrap();
        let occ = s.occurrences(a, b, 10);
        assert_eq!(occ, vec![Utc.with_ymd_and_hms(2026, 3, 2, 2, 0, 0).unwrap()]);
        let s = parse("CRON_TZ=Europe/Stockholm 0 3 * * *", None).unwrap();
        assert_eq!(s.occurrences(a, b, 10), occ);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse("not a cron", None).is_err());
        assert!(parse("0 3 * * *", Some("Mars/Base")).is_err());
        assert!(parse("0 0 3 * * *", None).is_err(), "seconds field not allowed");
    }

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    #[test]
    fn day_of_month_and_weekday_are_ored_like_kubernetes() {
        // April 2026: the 13th is a Monday, Fridays are 3, 10, 17, 24.
        let s = parse("0 0 13 * 5", None).unwrap();
        let occ = s.occurrences(utc(2026, 4, 1, 0, 0), utc(2026, 5, 1, 0, 0), 100);
        let days: Vec<u32> = occ.iter().map(|t| t.day()).collect();
        assert_eq!(days, vec![3, 10, 13, 17, 24]);
    }

    #[test]
    fn ranges_lists_and_names() {
        let week = (utc(2026, 3, 2, 0, 0), utc(2026, 3, 9, 0, 0)); // Mon..Mon
        let n = |p: &str| parse(p, None).unwrap().occurrences(week.0, week.1, 10_000).len();
        assert_eq!(n("0 9-17 * * 1-5"), 45);
        assert_eq!(n("0 9-17 * * MON-FRI"), 45);
        assert_eq!(n("0 0 * * mon,wed,fri"), 3);
        assert_eq!(n("0,30 * * * *"), 7 * 48);
        assert_eq!(n("0 0 * * 7"), 1, "7 is Sunday too");
        assert_eq!(n("0 0 * * 0"), 1);
    }

    #[test]
    fn macros_land_on_expected_times() {
        let from = utc(2026, 3, 2, 0, 0);
        let to = utc(2027, 3, 2, 0, 0);
        let first = |p: &str| parse(p, None).unwrap().occurrences(from, to, 10_000);
        let weekly = first("@weekly");
        assert_eq!(weekly.len(), 52);
        assert!(weekly.iter().all(|t| t.weekday() == chrono::Weekday::Sun && t.hour() == 0));
        assert_eq!(first("@monthly").len(), 12);
        assert_eq!(first("@yearly"), vec![utc(2027, 1, 1, 0, 0)]);
        assert_eq!(first("@annually"), first("@yearly"));
        assert_eq!(first("@DAILY").len(), 365, "macros are case-insensitive");
    }

    #[test]
    fn window_is_half_open_and_capped() {
        let s = parse("0 * * * *", None).unwrap();
        let (a, b) = (utc(2026, 3, 2, 1, 0), utc(2026, 3, 2, 3, 0));
        assert_eq!(s.occurrences(a, b, 100), vec![utc(2026, 3, 2, 1, 0), utc(2026, 3, 2, 2, 0)]);
        assert!(s.occurrences(a, a, 100).is_empty());
        assert_eq!(parse("* * * * *", None).unwrap().occurrences(a, b, 7).len(), 7);
        assert_eq!(parse("@every 1m", None).unwrap().occurrences(a, b, 7).len(), 7);
    }

    #[test]
    fn daylight_saving_transitions_run_once() {
        let s = parse("30 2 * * *", Some("Europe/Stockholm")).unwrap();
        // 2026-03-29: clocks jump 02:00 -> 03:00, so 02:30 does not exist.
        let spring = s.occurrences(utc(2026, 3, 28, 12, 0), utc(2026, 3, 29, 12, 0), 10);
        assert_eq!(spring.len(), 1);
        // 2026-10-25: 02:30 happens twice; the job still runs once.
        let autumn = s.occurrences(utc(2026, 10, 24, 12, 0), utc(2026, 10, 25, 12, 0), 10);
        assert_eq!(autumn, vec![utc(2026, 10, 25, 0, 30)]);
    }

    #[test]
    fn explicit_time_zone_wins_over_prefix() {
        let (a, b) = day();
        let s = parse("TZ=Asia/Tokyo 0 3 * * *", Some("UTC")).unwrap();
        assert_eq!(s.occurrences(a, b, 10), vec![utc(2026, 3, 2, 3, 0)]);
        let s = parse("TZ=Asia/Tokyo 0 3 * * *", None).unwrap();
        assert_eq!(s.occurrences(a, b, 10), vec![utc(2026, 3, 2, 18, 0)]);
        assert!(parse("CRON_TZ=Europe/Stockholm", None).is_err());
    }

    #[test]
    fn every_durations() {
        assert_eq!(parse_go_duration("90s"), Ok(Duration::seconds(90)));
        assert_eq!(parse_go_duration("1h30m"), Ok(Duration::minutes(90)));
        assert_eq!(parse_go_duration("2h"), Ok(Duration::hours(2)));
        for bad in ["", "5", "5x", "0s", "m", "1h30"] {
            assert!(parse_go_duration(bad).is_err(), "{bad:?} should be rejected");
        }
        assert!(parse("@every", None).is_err());
        assert!(parse("@every 5d", None).is_err());
    }

    #[test]
    fn rejects_out_of_range_fields() {
        for bad in ["61 * * * *", "0 24 * * *", "0 0 32 * *", "0 0 * 13 *", "0 0 * * 8", "* * * *", ""] {
            assert!(parse(bad, None).is_err(), "{bad:?} should be rejected");
        }
    }
}
