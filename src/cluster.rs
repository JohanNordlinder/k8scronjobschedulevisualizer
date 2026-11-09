//! Talking to the cluster through the user's kubeconfig.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use k8s_openapi::api::batch::v1::CronJob;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use kube::api::ListParams;
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Api, Client, Config};

use crate::model::CronJobInfo;

/// Path shown in the UI when none was given: `$KUBECONFIG` or `~/.kube/config`.
pub fn default_kubeconfig_path() -> String {
    if let Ok(p) = std::env::var("KUBECONFIG")
        && !p.is_empty() {
            return p;
        }
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".kube").join("config").display().to_string())
        .unwrap_or_default()
}

fn read_kubeconfig(path: &str) -> anyhow::Result<Kubeconfig> {
    let path = path.trim();
    // `$KUBECONFIG` may hold several files separated by ':' (';' on Windows).
    let sep = if cfg!(windows) { ';' } else { ':' };
    if path.is_empty() {
        return Kubeconfig::read().context("reading default kubeconfig");
    }
    let mut merged: Option<Kubeconfig> = None;
    for p in path.split(sep).filter(|p| !p.is_empty()) {
        let kc = Kubeconfig::read_from(Path::new(p)).with_context(|| format!("reading {p}"))?;
        merged = Some(match merged {
            None => kc,
            Some(m) => m.merge(kc)?,
        });
    }
    merged.context("no kubeconfig path given")
}

pub struct Contexts {
    pub names: Vec<String>,
    pub current: Option<String>,
}

pub fn list_contexts(path: &str) -> anyhow::Result<Contexts> {
    let kc = read_kubeconfig(path)?;
    Ok(Contexts {
        names: kc.contexts.iter().map(|c| c.name.clone()).collect(),
        current: kc.current_context,
    })
}

/// Fetch every CronJob visible in `namespace` (or the whole cluster if `None`).
pub async fn fetch_cronjobs(
    path: &str,
    context: Option<String>,
    namespace: Option<String>,
) -> anyhow::Result<Vec<CronJobInfo>> {
    let kc = read_kubeconfig(path)?;
    let opts = KubeConfigOptions { context, ..Default::default() };
    let config = Config::from_custom_kubeconfig(kc, &opts)
        .await
        .context("building client config from kubeconfig")?;
    let client = Client::try_from(config).context("creating client")?;

    let api: Api<CronJob> = match &namespace {
        Some(ns) => Api::namespaced(client, ns),
        None => Api::all(client),
    };

    let mut out = Vec::new();
    let mut params = ListParams::default().limit(500);
    loop {
        let list = api.list(&params).await.map_err(|e| {
            let hint = if namespace.is_none() {
                " (listing across all namespaces needs cluster-wide read access to cronjobs; try a single namespace)"
            } else {
                ""
            };
            anyhow::anyhow!("listing cronjobs failed: {e}{hint}")
        })?;
        out.extend(list.items.into_iter().map(convert));
        match list.metadata.continue_ {
            Some(token) if !token.is_empty() => params = params.continue_token(&token),
            _ => break,
        }
    }
    out.sort_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));
    Ok(out)
}

fn to_chrono(t: &Time) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(t.0.as_second(), 0)
}

fn convert(cj: CronJob) -> CronJobInfo {
    let spec = cj.spec.unwrap_or_default();
    let status = cj.status.unwrap_or_default();
    let mut info = CronJobInfo::new(
        cj.metadata.namespace.unwrap_or_default(),
        cj.metadata.name.unwrap_or_default(),
        spec.schedule,
        spec.time_zone,
    );
    info.suspended = spec.suspend.unwrap_or(false);
    info.last_schedule = status.last_schedule_time.as_ref().and_then(to_chrono);
    info.last_successful = status.last_successful_time.as_ref().and_then(to_chrono);
    info.active = status.active.map(|a| a.len()).unwrap_or(0);
    info
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// Writes `contents` to a fresh file in the temp dir and returns its path.
    fn temp_file(contents: &str) -> String {
        static N: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "cronscope-test-{}-{}.yaml",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::write(&path, contents).unwrap();
        path.display().to_string()
    }

    fn kubeconfig(server: &str, contexts: &[&str], current: &str) -> String {
        let ctxs: String = contexts
            .iter()
            .map(|c| format!("- name: {c}\n  context: {{cluster: c, user: u, namespace: default}}\n"))
            .collect();
        format!(
            "apiVersion: v1\nkind: Config\nclusters:\n- name: c\n  cluster: {{server: '{server}'}}\n\
             users:\n- name: u\n  user: {{token: sekrit}}\ncontexts:\n{ctxs}current-context: {current}\n"
        )
    }

    type RequestLog = Arc<Mutex<Vec<(String, String)>>>;

    /// A tiny fake API server. `respond(request_line)` returns (status, json body).
    /// Every request line and its Authorization header are recorded.
    fn fake_api(
        respond: impl Fn(&str) -> (u16, String) + Send + 'static,
    ) -> (String, RequestLog) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let log = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                let mut auth = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("authorization:") {
                        auth = v.trim().to_string();
                    }
                }
                let request = request.trim().to_string();
                let (status, body) = respond(&request);
                log2.lock().unwrap().push((request, auth));
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (url, log)
    }

    fn cronjob(ns: &str, name: &str, extra_spec: &str, status: &str) -> String {
        format!(
            r#"{{"metadata":{{"name":"{name}","namespace":"{ns}"}},
                "spec":{{"schedule":"0 * * * *","jobTemplate":{{}}{extra_spec}}},
                "status":{status}}}"#
        )
    }

    fn list(items: &[String], cont: Option<&str>) -> String {
        let cont = cont.map(|c| format!(r#","continue":"{c}""#)).unwrap_or_default();
        format!(
            r#"{{"apiVersion":"batch/v1","kind":"CronJobList","metadata":{{"resourceVersion":"1"{cont}}},"items":[{}]}}"#,
            items.join(",")
        )
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
    }

    #[test]
    fn lists_contexts_and_merges_files() {
        let a = temp_file(&kubeconfig("https://a.example:6443", &["dev", "test"], "test"));
        let c = list_contexts(&a).unwrap();
        assert_eq!(c.names, vec!["dev", "test"]);
        assert_eq!(c.current.as_deref(), Some("test"));

        let b = temp_file(&kubeconfig("https://b.example:6443", &["prod"], "prod"));
        let sep = if cfg!(windows) { ";" } else { ":" };
        let c = list_contexts(&format!("{a}{sep}{b}")).unwrap();
        assert_eq!(c.names, vec!["dev", "test", "prod"]);
        assert_eq!(c.current.as_deref(), Some("test"), "first file wins, like kubectl");
    }

    #[test]
    fn missing_or_broken_kubeconfig_is_an_error() {
        assert!(list_contexts("/definitely/not/here.yaml").is_err());
        assert!(list_contexts(&temp_file("this: [is not a kubeconfig")).is_err());
    }

    #[test]
    fn fetches_all_pages_with_token_and_maps_status() {
        let (url, log) = fake_api(|req| {
            if req.contains("continue=page2") {
                (200, list(&[cronjob("ns-b", "beta", "", "{}")], None))
            } else {
                let status = r#"{"lastScheduleTime":"2026-03-02T10:00:00Z",
                                 "lastSuccessfulTime":"2026-03-02T10:01:30Z",
                                 "active":[{"name":"alpha-1"},{"name":"alpha-2"}]}"#;
                let spec = r#","timeZone":"Europe/Stockholm","suspend":true"#;
                (200, list(&[cronjob("ns-a", "alpha", spec, status)], Some("page2")))
            }
        });
        let path = temp_file(&kubeconfig(&url, &["ctx"], "ctx"));

        let jobs = block_on(fetch_cronjobs(&path, Some("ctx".into()), None)).unwrap();

        let log = log.lock().unwrap();
        assert_eq!(log.len(), 2, "follows the continue token: {log:?}");
        assert!(log[0].0.starts_with("GET /apis/batch/v1/cronjobs?"), "{}", log[0].0);
        assert!(log.iter().all(|(_, auth)| auth == "bearer sekrit"));

        let names: Vec<String> = jobs.iter().map(|j| j.full_name()).collect();
        assert_eq!(names, vec!["ns-a/alpha", "ns-b/beta"]);
        let a = &jobs[0];
        assert!(a.suspended);
        assert_eq!(a.time_zone.as_deref(), Some("Europe/Stockholm"));
        assert_eq!(a.active, 2);
        assert_eq!(a.estimated_duration(), Some(chrono::Duration::seconds(90)));
        assert!(a.parsed.is_ok());
        let b = &jobs[1];
        assert!(!b.suspended && b.last_schedule.is_none() && b.active == 0);
    }

    #[test]
    fn namespace_uses_namespaced_path() {
        let (url, log) = fake_api(|_| (200, list(&[], None)));
        let path = temp_file(&kubeconfig(&url, &["ctx"], "ctx"));
        let jobs = block_on(fetch_cronjobs(&path, None, Some("billing".into()))).unwrap();
        assert!(jobs.is_empty());
        let req = &log.lock().unwrap()[0].0;
        assert!(req.starts_with("GET /apis/batch/v1/namespaces/billing/cronjobs?"), "{req}");
    }

    #[test]
    fn forbidden_cluster_wide_list_suggests_a_namespace() {
        let forbidden = r#"{"kind":"Status","apiVersion":"v1","status":"Failure","reason":"Forbidden",
            "message":"cronjobs.batch is forbidden: User \"dev\" cannot list resource \"cronjobs\" at the cluster scope","code":403}"#;
        let (url, _) = fake_api(move |_| (403, forbidden.to_string()));
        let path = temp_file(&kubeconfig(&url, &["ctx"], "ctx"));

        let err = block_on(fetch_cronjobs(&path, None, None)).unwrap_err().to_string();
        assert!(err.contains("forbidden") && err.contains("try a single namespace"), "{err}");

        let err = block_on(fetch_cronjobs(&path, None, Some("x".into()))).unwrap_err().to_string();
        assert!(!err.contains("try a single namespace"), "{err}");
    }

    #[test]
    fn unknown_context_is_an_error() {
        let path = temp_file(&kubeconfig("https://a.example:6443", &["ctx"], "ctx"));
        assert!(block_on(fetch_cronjobs(&path, Some("nope".into()), None)).is_err());
    }

    #[test]
    fn empty_cronjob_object_converts() {
        let j = convert(CronJob::default());
        assert_eq!(j.full_name(), "/");
        assert!(j.parsed.is_err(), "empty schedule is reported as invalid");
        assert!(!j.suspended);
    }
}
