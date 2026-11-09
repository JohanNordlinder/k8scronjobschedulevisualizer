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
