# cronscope

A local desktop tool that reads every CronJob on an OpenShift (or any Kubernetes) cluster and shows **when they fire and where they pile up**. It's written in Rust with [egui](https://github.com/emilk/egui).

It only reads. It lists `batch/v1` CronJobs and changes nothing on the cluster.

## Views

- **Heatmap:** a 24 × 60 grid with one cell per minute of the day. A cell's colour shows how many jobs start in that minute, or how many are running if you pick *Est. running*. The bar strip underneath adds up each minute of the hour across all hours, which shows "everything runs at :00" problems at a glance. Hover a cell to see its jobs. Click it to list them in the side panel.
- **Timeline:** a bar chart covering the whole window (24 h or 7 days), so you can spot weekly or business-hours peaks. Scroll to zoom and drag to pan.
- **Jobs:** a sortable table with schedule, time zone, number of runs in the window, next run, estimated run time and status. Click a job name to outline its start minutes on the heatmap.
- **Name filter:** in the top bar, or `--filter` on the command line. It matches `namespace/name`, ignoring case. Separate terms with commas to keep jobs matching any of them, and prefix a term with `!` to hide jobs containing it, e.g. `backup, sync, !test`. Hidden jobs are left out of the heatmap, timeline, hotspots and job table.
- **Side panel:** shows the peak minute and the busiest times of day. Selecting a cell shows which jobs fire in it.

**Est. running** works out how many jobs overlap. It takes each job's run time from `status.lastSuccessfulTime - status.lastScheduleTime`. If a job has no finished run yet, it uses the *Unknown run time* setting, which defaults to 1 minute.

## Schedule semantics

These follow the Kubernetes CronJob controller:

- standard 5-field cron, with day-of-month and day-of-week combined using OR (Vixie cron behaviour)
- `@hourly`, `@daily`/`@midnight`, `@weekly`, `@monthly`, `@yearly`, and `@every 1h30m`
- `spec.timeZone`, plus the legacy `CRON_TZ=…`/`TZ=…` prefix. Jobs without a time zone are treated as **UTC**, which is the kube-controller-manager default on OpenShift.
- suspended jobs are left out unless you tick *Include suspended*. Invalid schedules are counted and shown in the Jobs table.

Times can be shown in your local time zone or in UTC.

## Running

You need a Rust toolchain (1.85 or newer, edition 2024).

```bash
cargo run --release
```

```bash
cargo run --release -- --context my-cluster -n my-namespace
```

```bash
cargo run --release -- --demo
```

```bash
cargo run --release -- --context my-cluster --filter "backup, !test"
```

Authentication comes from your kubeconfig, so `oc login` first. The tool uses `--kubeconfig <path>` if given, otherwise `$KUBECONFIG` (several files may be listed), otherwise `~/.kube/config`. You can also edit the path, pick a context and set a namespace in the UI, then press **Fetch**. Tokens, client certificates and exec plugins all work.

Leaving the namespace empty lists CronJobs across all namespaces. That needs cluster-wide `list` on `cronjobs`, for example the `cluster-reader` role. Without it, enter a namespace.

On Linux, a desktop session normally already has the X11/Wayland and Vulkan/GL libraries it needs. A minimal system may need `libxkbcommon-x11-0` and a Vulkan or GL driver.

## Development

```bash
cargo test
```

| File | Purpose |
|---|---|
| `src/schedule.rs` | Cron parsing and expansion, with time zones |
| `src/cluster.rs` | Reading the kubeconfig and listing CronJobs with kube-rs |
| `src/analysis.rs` | Per-minute counts, concurrency, heatmap and hotspots |
| `src/app.rs` | egui user interface |
| `src/demo.rs` | Generated demo data |
