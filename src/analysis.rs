//! Turns a list of CronJobs into per-minute load figures and hotspots.

use chrono::{DateTime, Duration, Local, Timelike, Utc};

use crate::model::{CronJobInfo, NameFilter};

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
    /// See [`NameFilter`]; jobs that don't match are left out entirely.
    pub name_filter: String,
}

#[derive(Clone, Debug, Default)]
pub struct JobStats {
    /// Matches the name filter (shown in the job table).
    pub matches_filter: bool,
    /// Counted in the load figures.
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
    pub filtered_out: usize,
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
            filtered_out: 0,
            params: params.clone(),
        };

        fn bump(cell: &mut CellJobs, job: usize) {
            match cell.last_mut() {
                Some((j, c)) if *j == job => *c += 1,
                _ => cell.push((job, 1)),
            }
        }

        let filter = NameFilter::parse(&params.name_filter);
        for (ji, job) in jobs.iter().enumerate() {
            let mut st = JobStats::default();
            if !filter.matches(job) {
                a.filtered_out += 1;
                a.jobs.push(st);
                continue;
            }
            st.matches_filter = true;
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
                name_filter: String::new(),
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

    fn params(start: DateTime<Utc>, days: usize) -> Params {
        Params {
            window_start: start,
            window_minutes: days * MINUTES_PER_DAY,
            display: DisplayTz::Utc,
            include_suspended: false,
            default_duration_min: 1,
            name_filter: String::new(),
        }
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, 2, 0, 0, 0).unwrap()
    }

    #[test]
    fn suspended_jobs_are_optional() {
        let mut s = job("0 * * * *");
        s.suspended = true;
        let jobs = vec![s, job("30 * * * *")];
        let a = Analysis::compute(&jobs, params(t0(), 1));
        assert_eq!((a.suspended_skipped, a.starts[0], a.starts[30]), (1, 0, 1));
        assert!(!a.jobs[0].included && a.jobs[0].matches_filter);
        assert!(a.jobs[0].next_run.is_some(), "next run is still shown for suspended jobs");

        let a = Analysis::compute(&jobs, Params { include_suspended: true, ..params(t0(), 1) });
        assert_eq!((a.suspended_skipped, a.starts[0]), (0, 1));
    }

    #[test]
    fn name_filter_removes_jobs_from_all_figures() {
        let jobs = vec![
            CronJobInfo::new("billing", "backup", "0 * * * *", None),
            CronJobInfo::new("billing", "backup-test", "0 * * * *", None),
            CronJobInfo::new("search", "reindex", "0 * * * *", None),
            CronJobInfo::new("search", "broken", "nope", None),
        ];
        let a = Analysis::compute(&jobs, Params { name_filter: "backup, !test".into(), ..params(t0(), 1) });
        assert_eq!(a.filtered_out, 3);
        assert_eq!(a.invalid, 0, "filtered-out invalid jobs are not reported as invalid");
        assert_eq!(a.starts[0], 1);
        assert_eq!(a.cell_starts[0], vec![(0, 1)]);
        let shown: Vec<bool> = a.jobs.iter().map(|j| j.matches_filter).collect();
        assert_eq!(shown, vec![true, false, false, false]);
    }

    #[test]
    fn runs_started_before_the_window_still_count_as_running() {
        // Runs at 23:50 for 30 minutes, so it is still running for the first 20 minutes.
        let mut j = job("50 23 * * *");
        j.last_schedule = Some(t0());
        j.last_successful = Some(t0() + Duration::minutes(30));
        let a = Analysis::compute(&[j], params(t0(), 1));
        assert_eq!(a.running[..20], [1; 20]);
        assert_eq!(a.running[20], 0);
        assert_eq!(a.starts[0], 0, "the start itself is outside the window");
        // The run that starts at 23:50 inside the window is cut off at the end.
        assert_eq!(a.running[MINUTES_PER_DAY - 10..], [1; 10]);
        assert_eq!(a.jobs[0].runs, 1);
    }

    #[test]
    fn default_duration_applies_without_history() {
        let a = Analysis::compute(&[job("0 0 * * *")], Params { default_duration_min: 5, ..params(t0(), 1) });
        assert_eq!(a.running[..6], [1, 1, 1, 1, 1, 0]);
        assert!(!a.jobs[0].duration_estimated);
        assert_eq!(a.jobs[0].duration_min, 5);
    }

    #[test]
    fn week_window_averages_per_day() {
        let jobs = vec![job("15 4 * * *"), job("0 12 * * 1")];
        let a = Analysis::compute(&jobs, params(t0(), 7));
        let heat = a.heat(Metric::Starts);
        assert_eq!(heat[4 * 60 + 15], 1.0, "daily job: one start per day on average");
        assert!((heat[12 * 60] - 1.0 / 7.0).abs() < 1e-6, "weekly job: 1/7 per day");
        assert_eq!(a.cell_days.iter().copied().collect::<std::collections::HashSet<_>>().len(), 1);
        assert_eq!(a.cell_days[0], 7);
        assert_eq!(a.jobs[0].runs, 7);
        assert_eq!(a.jobs[0].start_cells, vec![4 * 60 + 15], "start cells are de-duplicated");
    }

    #[test]
    fn window_not_aligned_to_midnight() {
        let start = t0() + Duration::minutes(10 * 60 + 7);
        let a = Analysis::compute(&[job("0 9 * * *")], params(start, 1));
        // 09:00 happens on the next day, 23h53m into the window.
        assert_eq!(a.starts.iter().position(|&v| v > 0), Some(23 * 60 - 7));
        assert_eq!(a.heat(Metric::Starts)[9 * 60], 1.0);
        assert_eq!(a.minute_time(0), start);
    }

    #[test]
    fn hotspots_are_sorted_truncated_and_skip_zero() {
        let mut heat = vec![0.0; MINUTES_PER_DAY];
        heat[10] = 2.0;
        heat[5] = 3.0;
        heat[20] = 2.0;
        let a = Analysis::compute(&[], params(t0(), 1));
        assert_eq!(a.hotspots(&heat, 10), vec![(5, 3.0), (10, 2.0), (20, 2.0)]);
        assert_eq!(a.hotspots(&heat, 2), vec![(5, 3.0), (10, 2.0)]);
        assert!(a.hotspots(&vec![0.0; MINUTES_PER_DAY], 5).is_empty());
    }

    #[test]
    fn running_cells_track_which_jobs_overlap() {
        let mut long = job("0 1 * * *");
        long.last_schedule = Some(t0());
        long.last_successful = Some(t0() + Duration::minutes(3));
        let jobs = vec![long, job("2 1 * * *")];
        let a = Analysis::compute(&jobs, params(t0(), 1));
        assert_eq!(a.cell_jobs(Metric::Running, 60 + 2), &vec![(0, 1), (1, 1)]);
        assert_eq!(a.cell_jobs(Metric::Starts, 60 + 2), &vec![(1, 1)]);
        assert_eq!(a.series(Metric::Running)[60..64], [1, 1, 2, 0]);
    }
}
