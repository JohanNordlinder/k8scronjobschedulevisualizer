//! Turns a list of CronJobs into per-minute load figures and hotspots.

use chrono::{DateTime, Duration, Local, Timelike, Utc};

use crate::model::CronJobInfo;

pub const MINUTES_PER_DAY: usize = 24 * 60;
/// Upper bound on occurrences expanded per job (a `* * * * *` job over 8 days fits).
const MAX_OCCURRENCES: usize = 12_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayTz {
    Local,
    Utc,
}

impl DisplayTz {
    pub fn format(self, t: DateTime<Utc>, fmt: &str) -> String {
        match self {
            DisplayTz::Utc => t.format(fmt).to_string(),
            DisplayTz::Local => t.with_timezone(&Local).format(fmt).to_string(),
        }
    }

    /// Minute of the day (0..1440) of `t` in this zone.
    pub fn minute_of_day(self, t: DateTime<Utc>) -> usize {
        let (h, m) = match self {
            DisplayTz::Utc => (t.hour(), t.minute()),
            DisplayTz::Local => {
                let l = t.with_timezone(&Local);
                (l.hour(), l.minute())
            }
        };
        (h * 60 + m) as usize
    }

    pub fn label(self) -> &'static str {
        match self {
            DisplayTz::Local => "Local",
            DisplayTz::Utc => "UTC",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    /// Number of jobs started in a minute.
    Starts,
    /// Number of jobs estimated to be running in a minute.
    Running,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    pub window_start: DateTime<Utc>,
    pub window_minutes: usize,
    pub display: DisplayTz,
    pub include_suspended: bool,
    /// Assumed run time when the status has no finished run to learn from.
    pub default_duration_min: u32,
}

#[derive(Clone, Debug, Default)]
pub struct JobStats {
    pub included: bool,
    pub runs: usize,
    pub next_run: Option<DateTime<Utc>>,
    pub duration_min: u32,
    pub duration_estimated: bool,
    /// Minutes of the day (display zone) at which this job starts.
    pub start_cells: Vec<u16>,
}

/// Jobs touching one minute-of-day cell: (job index, occurrences in window).
pub type CellJobs = Vec<(usize, u32)>;

pub struct Analysis {
    pub params: Params,
    /// Per absolute minute of the window.
    pub starts: Vec<u32>,
    pub running: Vec<u32>,
    /// How often each minute-of-day occurs in the window (used for averaging).
    pub cell_days: Vec<u32>,
    pub cell_starts: Vec<CellJobs>,
    pub cell_running: Vec<CellJobs>,
    pub jobs: Vec<JobStats>,
    pub invalid: usize,
    pub suspended_skipped: usize,
}

impl Analysis {
    pub fn compute(jobs: &[CronJobInfo], params: Params) -> Self {
        let n = params.window_minutes;
        let start = params.window_start;
        let end = start + Duration::minutes(n as i64);

        let minute_cell: Vec<usize> = (0..n)
            .map(|i| params.display.minute_of_day(start + Duration::minutes(i as i64)))
            .collect();
        let mut cell_days = vec![0u32; MINUTES_PER_DAY];
        for &c in &minute_cell {
            cell_days[c] += 1;
        }

        let mut a = Analysis {
            starts: vec![0; n],
            running: vec![0; n],
            cell_days,
            cell_starts: vec![Vec::new(); MINUTES_PER_DAY],
            cell_running: vec![Vec::new(); MINUTES_PER_DAY],
            jobs: Vec::with_capacity(jobs.len()),
            invalid: 0,
            suspended_skipped: 0,
            params: params.clone(),
        };

        fn bump(cell: &mut CellJobs, job: usize) {
            match cell.last_mut() {
                Some((j, c)) if *j == job => *c += 1,
                _ => cell.push((job, 1)),
            }
        }

        for (ji, job) in jobs.iter().enumerate() {
            let mut st = JobStats::default();
            let Ok(sched) = &job.parsed else {
                a.invalid += 1;
                a.jobs.push(st);
                continue;
            };
            match job.estimated_duration() {
                Some(d) => {
                    st.duration_min = ((d.num_seconds() + 59) / 60).max(1) as u32;
                    st.duration_estimated = true;
                }
                None => st.duration_min = params.default_duration_min.max(1),
            }
            st.next_run = sched.occurrences(Utc::now(), Utc::now() + Duration::days(400), 1).first().copied();

            if job.suspended && !params.include_suspended {
                a.suspended_skipped += 1;
                a.jobs.push(st);
                continue;
            }
            st.included = true;

            // Look back far enough to catch runs that started before the window
            // but are still going when it opens.
            let lookback = Duration::minutes(st.duration_min as i64 - 1);
            for t in sched.occurrences(start - lookback, end, MAX_OCCURRENCES) {
                let offset = (t - start).num_minutes();
                if offset >= 0 {
                    let i = offset as usize;
                    a.starts[i] += 1;
                    st.runs += 1;
                    let cell = minute_cell[i];
                    bump(&mut a.cell_starts[cell], ji);
                    if !st.start_cells.contains(&(cell as u16)) {
                        st.start_cells.push(cell as u16);
                    }
                }
                let from = offset.max(0) as usize;
                let to = ((offset + st.duration_min as i64).max(0) as usize).min(n);
                for i in from..to {
                    a.running[i] += 1;
                    bump(&mut a.cell_running[minute_cell[i]], ji);
                }
            }
            a.jobs.push(st);
        }
        a
    }

    pub fn series(&self, metric: Metric) -> &[u32] {
        match metric {
            Metric::Starts => &self.starts,
            Metric::Running => &self.running,
        }
    }

    pub fn cell_jobs(&self, metric: Metric, cell: usize) -> &CellJobs {
        match metric {
            Metric::Starts => &self.cell_starts[cell],
            Metric::Running => &self.cell_running[cell],
        }
    }

    /// Average value per day for every minute of the day.
    pub fn heat(&self, metric: Metric) -> Vec<f32> {
        let mut sums = vec![0u32; MINUTES_PER_DAY];
        for (i, v) in self.series(metric).iter().enumerate() {
            let cell = self
                .params
                .display
                .minute_of_day(self.params.window_start + Duration::minutes(i as i64));
            sums[cell] += v;
        }
        sums.iter()
            .zip(&self.cell_days)
            .map(|(&s, &d)| if d == 0 { 0.0 } else { s as f32 / d as f32 })
            .collect()
    }

    /// The busiest minutes of the day, highest first.
    pub fn hotspots(&self, heat: &[f32], count: usize) -> Vec<(usize, f32)> {
        let mut cells: Vec<(usize, f32)> = heat.iter().copied().enumerate().filter(|(_, v)| *v > 0.0).collect();
        cells.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        cells.truncate(count);
        cells
    }

    pub fn minute_time(&self, i: usize) -> DateTime<Utc> {
        self.params.window_start + Duration::minutes(i as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn job(s: &str) -> CronJobInfo {
        CronJobInfo::new("ns", s, s, None)
    }

    #[test]
    fn counts_and_concurrency() {
        let start = Utc.with_ymd_and_hms(2026, 3, 2, 0, 0, 0).unwrap();
        let mut slow = job("0 * * * *");
        slow.last_schedule = Some(start);
        slow.last_successful = Some(start + Duration::minutes(10));
        let jobs = vec![slow, job("0 * * * *"), job("*/30 * * * *"), job("bogus")];
        let a = Analysis::compute(
            &jobs,
            Params {
                window_start: start,
                window_minutes: MINUTES_PER_DAY,
                display: DisplayTz::Utc,
                include_suspended: false,
                default_duration_min: 1,
            },
        );
        assert_eq!(a.invalid, 1);
        assert_eq!(a.starts[0], 3);
        assert_eq!(a.starts[30], 1);
        assert_eq!(a.running[5], 1, "only the 10-minute job is still running");
        assert_eq!(a.jobs[0].runs, 24);
        let heat = a.heat(Metric::Starts);
        assert_eq!(a.hotspots(&heat, 1), vec![(0, 3.0)]);
        assert_eq!(a.cell_starts[60], vec![(0, 1), (1, 1), (2, 1)]);
    }
}
