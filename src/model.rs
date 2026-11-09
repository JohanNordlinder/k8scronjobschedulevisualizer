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
