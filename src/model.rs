use chrono::{DateTime, Duration, Utc};

use crate::schedule::{self, Schedule};

/// What we need to know about one CronJob.
#[derive(Clone, Debug)]
pub struct CronJobInfo {
    pub namespace: String,
    pub name: String,
    pub schedule: String,
    pub time_zone: Option<String>,
    pub suspended: bool,
    pub last_schedule: Option<DateTime<Utc>>,
    pub last_successful: Option<DateTime<Utc>>,
    pub active: usize,
    pub parsed: Result<Schedule, String>,
}

impl CronJobInfo {
    pub fn new(
        namespace: impl Into<String>,
        name: impl Into<String>,
        schedule: impl Into<String>,
        time_zone: Option<String>,
    ) -> Self {
        let schedule = schedule.into();
        let parsed = schedule::parse(&schedule, time_zone.as_deref());
        Self {
            namespace: namespace.into(),
            name: name.into(),
            schedule,
            time_zone,
            suspended: false,
            last_schedule: None,
            last_successful: None,
            active: 0,
            parsed,
        }
    }

    pub fn full_name(&self) -> String {
        format!("{}/{}", self.namespace, self.name)
    }

    /// Run time of the last successful run, if the status shows one that
    /// finished after it was scheduled.
    pub fn estimated_duration(&self) -> Option<Duration> {
        let (s, e) = (self.last_schedule?, self.last_successful?);
        let d = e - s;
        (d >= Duration::zero() && d < Duration::days(1)).then_some(d)
    }
}

/// Filter on `namespace/name`, case-insensitive.
///
/// Terms are separated by commas. A job is kept if it contains at least one
/// plain term (or there are none) and none of the `!`-prefixed terms.
/// Example: `backup, sync, !test`.
#[derive(Clone, Debug, Default)]
pub struct NameFilter {
    include: Vec<String>,
    exclude: Vec<String>,
}

impl NameFilter {
    pub fn parse(s: &str) -> Self {
        let mut f = NameFilter::default();
        for term in s.split(',').map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()) {
            match term.strip_prefix('!') {
                Some(rest) if !rest.trim().is_empty() => f.exclude.push(rest.trim().to_string()),
                Some(_) => {}
                None => f.include.push(term),
            }
        }
        f
    }

    pub fn matches(&self, job: &CronJobInfo) -> bool {
        let name = job.full_name().to_lowercase();
        (self.include.is_empty() || self.include.iter().any(|t| name.contains(t.as_str())))
            && !self.exclude.iter().any(|t| name.contains(t.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_filter() {
        let job = |ns: &str, n: &str| CronJobInfo::new(ns, n, "@daily", None);
        let f = NameFilter::parse(" Backup , sync, !test ");
        assert!(f.matches(&job("billing", "db-backup")));
        assert!(f.matches(&job("sync-team", "x")), "namespace is matched too");
        assert!(!f.matches(&job("billing", "backup-test")));
        assert!(!f.matches(&job("billing", "cleanup")));
        assert!(NameFilter::parse("").matches(&job("a", "b")));
        assert!(!NameFilter::parse("!b").matches(&job("a", "b")));
        assert!(NameFilter::parse("!").matches(&job("a", "b")));
    }
}
