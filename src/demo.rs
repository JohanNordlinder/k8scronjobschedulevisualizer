//! Made-up CronJobs so the UI can be explored without a cluster (`--demo`).

use chrono::{Duration, DurationRound, Utc};

use crate::model::CronJobInfo;

/// Small deterministic PRNG so the demo looks the same every run.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

pub fn jobs() -> Vec<CronJobInfo> {
    let namespaces = [
        "payments", "billing", "search", "reporting", "identity", "logistics", "platform-ops", "data-lake",
    ];
    let names = [
        "cleanup", "sync", "export", "reindex", "backup", "rotate-keys", "digest-mail", "compact",
        "refresh-cache", "reconcile", "vacuum", "snapshot", "metrics-rollup", "purge-sessions",
    ];
    // (schedule, weight, typical duration in seconds). Heavy weight on the
    // classic "everyone picks the top of the hour" patterns.
    let patterns: &[(&str, u64, i64)] = &[
        ("0 * * * *", 14, 240),
        ("*/5 * * * *", 8, 40),
        ("*/15 * * * *", 6, 90),
        ("0 0 * * *", 10, 1500),
        ("@daily", 4, 900),
        ("0 2 * * *", 6, 2400),
        ("30 2 * * *", 3, 1200),
        ("0 */6 * * *", 5, 600),
        ("*/10 * * * *", 4, 120),
        ("0 3 * * 1", 2, 3600),
        ("17 * * * *", 2, 60),
        ("42 4 * * *", 2, 300),
        ("0 9-17 * * 1-5", 3, 180),
        ("*/1 * * * *", 2, 15),
        ("0 6 * * *", 3, 600),
    ];
    let total: u64 = patterns.iter().map(|p| p.1).sum();
    let mut rng = Lcg(0xC0FFEE);
    let now = Utc::now().duration_trunc(Duration::minutes(1)).unwrap();

    let mut out = Vec::new();
    for (i, ns) in namespaces.iter().enumerate() {
        let count = 8 + rng.below(10) as usize;
        for j in 0..count {
            let mut pick = rng.below(total);
            let &(sched, _, dur) = patterns
                .iter()
                .find(|p| {
                    if pick < p.1 {
                        true
                    } else {
                        pick -= p.1;
                        false
                    }
                })
                .unwrap();
            let name = format!("{}-{}", names[(i * 3 + j) % names.len()], j);
            let tz = (ns == &"logistics" && j % 2 == 0).then(|| "Europe/Stockholm".to_string());
            let mut job = CronJobInfo::new(*ns, name, sched, tz);
            job.suspended = rng.below(15) == 0;
            let jitter = rng.below(dur as u64 + 1) as i64 - dur / 2;
            let last = now - Duration::minutes(rng.below(60) as i64 + 1);
            job.last_schedule = Some(last);
            job.last_successful = Some(last + Duration::seconds((dur + jitter).max(5)));
            out.push(job);
        }
    }
    out.push(CronJobInfo::new("platform-ops", "broken-schedule", "61 * * * *", None));
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn demo_data_is_stable_and_mostly_valid() {
        let a = super::jobs();
        let b = super::jobs();
        assert!(a.len() > 50);
        let names = |v: &[crate::model::CronJobInfo]| v.iter().map(|j| j.full_name()).collect::<Vec<_>>();
        assert_eq!(names(&a), names(&b), "deterministic between runs");
        let invalid: Vec<_> = a.iter().filter(|j| j.parsed.is_err()).map(|j| j.name.as_str()).collect();
        assert_eq!(invalid, vec!["broken-schedule"]);
        assert!(a.iter().any(|j| j.suspended));
        assert!(a.iter().any(|j| j.time_zone.is_some()));
        let unique: std::collections::HashSet<_> = names(&a).into_iter().collect();
        assert_eq!(unique.len(), a.len(), "no duplicate namespace/name");
    }
}
