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
    use chrono::TimeZone;

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
}
