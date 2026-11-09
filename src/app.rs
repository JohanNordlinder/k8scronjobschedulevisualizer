use std::sync::mpsc::{Receiver, TryRecvError, channel};

use chrono::{DateTime, Duration, DurationRound, Utc};
use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2,
};
use egui_plot::{Bar, BarChart, Plot};

use crate::analysis::{Analysis, DisplayTz, MINUTES_PER_DAY, Metric, Params};
use crate::cluster;
use crate::demo;
use crate::model::CronJobInfo;

pub struct Options {
    pub kubeconfig: Option<String>,
    pub context: Option<String>,
    pub namespace: Option<String>,
    pub filter: Option<String>,
    pub demo: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Heatmap,
    Timeline,
    Jobs,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Name,
    Runs,
    Next,
    Duration,
}

type FetchResult = anyhow::Result<Vec<CronJobInfo>>;

pub struct App {
    // connection
    kubeconfig: String,
    contexts: Vec<String>,
    context: Option<String>,
    context_error: Option<String>,
    namespace: String,

    // data
    jobs: Vec<CronJobInfo>,
    source: String,
    loaded_at: Option<DateTime<Utc>>,
    loading: Option<Receiver<FetchResult>>,
    load_error: Option<String>,

    // view settings
    window_days: usize,
    display: DisplayTz,
    metric: Metric,
    include_suspended: bool,
    default_duration_min: u32,
    window_start: DateTime<Utc>,
    analysis: Option<Analysis>,

    // interaction
    tab: Tab,
    selected_cell: Option<usize>,
    selected_job: Option<usize>,
    name_filter: String,
    sort: SortKey,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: Options) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let mut app = App {
            kubeconfig: opts.kubeconfig.unwrap_or_else(cluster::default_kubeconfig_path),
            contexts: Vec::new(),
            context: opts.context.clone(),
            context_error: None,
            namespace: opts.namespace.unwrap_or_default(),
            jobs: Vec::new(),
            source: String::new(),
            loaded_at: None,
            loading: None,
            load_error: None,
            window_days: 1,
            display: DisplayTz::Local,
            metric: Metric::Starts,
            include_suspended: false,
            default_duration_min: 1,
            window_start: now_minute(),
            analysis: None,
            tab: Tab::Heatmap,
            selected_cell: None,
            selected_job: None,
            name_filter: opts.filter.unwrap_or_default(),
            sort: SortKey::Name,
        };
        if opts.demo {
            app.load_demo();
        } else {
            app.reload_contexts();
            if opts.context.is_some() {
                app.context = opts.context;
            }
            if app.context_error.is_none() {
                app.fetch(&cc.egui_ctx);
            }
        }
        app
    }

    fn reload_contexts(&mut self) {
        match cluster::list_contexts(&self.kubeconfig) {
            Ok(c) => {
                if self.context.as_ref().is_none_or(|cur| !c.names.contains(cur)) {
                    self.context = c.current.or_else(|| c.names.first().cloned());
                }
                self.contexts = c.names;
                self.context_error = None;
            }
            Err(e) => {
                self.contexts.clear();
                self.context_error = Some(format!("{e:#}"));
            }
        }
    }

    fn fetch(&mut self, ctx: &egui::Context) {
        let (tx, rx) = channel();
        let path = self.kubeconfig.clone();
        let context = self.context.clone();
        let ns = Some(self.namespace.trim().to_string()).filter(|s| !s.is_empty());
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(anyhow::Error::from)
                .and_then(|rt| rt.block_on(cluster::fetch_cronjobs(&path, context, ns)));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.loading = Some(rx);
        self.load_error = None;
    }

    fn load_demo(&mut self) {
        self.set_jobs(demo::jobs(), "demo data".into());
    }

    fn set_jobs(&mut self, jobs: Vec<CronJobInfo>, source: String) {
        self.jobs = jobs;
        self.source = source;
        self.loaded_at = Some(Utc::now());
        self.selected_job = None;
        self.selected_cell = None;
        self.window_start = now_minute();
        self.analysis = None;
    }

    fn poll_loading(&mut self) {
        let Some(rx) = &self.loading else { return };
        match rx.try_recv() {
            Ok(Ok(jobs)) => {
                let ns = if self.namespace.trim().is_empty() { "all namespaces" } else { self.namespace.trim() };
                let source = format!("{} · {}", self.context.as_deref().unwrap_or("current context"), ns);
                self.set_jobs(jobs, source);
                self.loading = None;
            }
            Ok(Err(e)) => {
                self.load_error = Some(format!("{e:#}"));
                self.loading = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.load_error = Some("fetch thread died".into());
                self.loading = None;
            }
        }
    }

    fn params(&self) -> Params {
        Params {
            window_start: self.window_start,
            window_minutes: self.window_days * MINUTES_PER_DAY,
            display: self.display,
            include_suspended: self.include_suspended,
            default_duration_min: self.default_duration_min,
            name_filter: self.name_filter.clone(),
        }
    }

    fn ensure_analysis(&mut self) {
        let params = self.params();
        if self.analysis.as_ref().is_none_or(|a| a.params != params) {
            self.analysis = Some(Analysis::compute(&self.jobs, params));
        }
    }

    // ───────────────────────────── UI ─────────────────────────────

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("cronscope").strong().size(16.0));
            ui.separator();
            ui.label("kubeconfig");
            let r = ui.add(egui::TextEdit::singleline(&mut self.kubeconfig).desired_width(220.0));
            if r.lost_focus() {
                self.reload_contexts();
            }
            ui.label("context");
            egui::ComboBox::from_id_salt("context")
                .width(220.0)
                .selected_text(self.context.clone().unwrap_or_else(|| "—".into()))
                .show_ui(ui, |ui| {
                    for c in &self.contexts {
                        ui.selectable_value(&mut self.context, Some(c.clone()), c);
                    }
                });
            ui.label("namespace");
            ui.add(
                egui::TextEdit::singleline(&mut self.namespace)
                    .hint_text("all namespaces")
                    .desired_width(140.0),
            );
            let busy = self.loading.is_some();
            if ui.add_enabled(!busy, egui::Button::new("⟳ Fetch")).clicked() {
                self.reload_contexts();
                self.fetch(ui.ctx());
            }
            if busy {
                ui.spinner();
            }
            if ui.button("Demo data").clicked() {
                self.load_demo();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Name filter");
            ui.add(
                egui::TextEdit::singleline(&mut self.name_filter)
                    .hint_text("e.g. backup, sync, !test")
                    .desired_width(220.0),
            )
            .on_hover_text(
                "Matches namespace/name, case-insensitive.\n\
                 Separate terms with commas: a job is shown if it contains any term.\n\
                 Prefix a term with ! to hide jobs containing it.",
            );
            if !self.name_filter.is_empty() && ui.small_button("✕").on_hover_text("Clear filter").clicked() {
                self.name_filter.clear();
            }
            ui.separator();
            ui.label("Window");
            ui.selectable_value(&mut self.window_days, 1, "24 h");
            ui.selectable_value(&mut self.window_days, 7, "7 days");
            ui.separator();
            ui.label("Times in");
            ui.selectable_value(&mut self.display, DisplayTz::Local, "Local");
            ui.selectable_value(&mut self.display, DisplayTz::Utc, "UTC");
            ui.separator();
            ui.label("Measure");
            ui.selectable_value(&mut self.metric, Metric::Starts, "Job starts")
                .on_hover_text("How many jobs are triggered in each minute");
            ui.selectable_value(&mut self.metric, Metric::Running, "Est. running")
                .on_hover_text(
                    "How many jobs are running in each minute, using each job's last successful run time",
                );
            ui.separator();
            ui.checkbox(&mut self.include_suspended, "Include suspended");
            ui.label("Unknown run time");
            ui.add(egui::DragValue::new(&mut self.default_duration_min).range(1..=240).suffix(" min"))
                .on_hover_text("Assumed run time for jobs whose status has no finished run yet");
            if ui.button("Now").on_hover_text("Move the window to start at the current minute").clicked() {
                self.window_start = now_minute();
            }
        });
        if let Some(e) = &self.context_error {
            ui.colored_label(ui.visuals().warn_fg_color, format!("kubeconfig: {e}"));
        }
        if let Some(e) = &self.load_error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let Some(a) = &self.analysis else {
                ui.label("No data loaded");
                return;
            };
            let included = a.jobs.iter().filter(|j| j.included).count();
            ui.label(format!(
                "{} · {} cronjobs ({} analyzed, {} hidden by filter, {} suspended skipped, {} invalid)",
                if self.source.is_empty() { "—" } else { &self.source },
                self.jobs.len(),
                included,
                a.filtered_out,
                a.suspended_skipped,
                a.invalid
            ));
            if let Some(t) = self.loaded_at {
                ui.separator();
                ui.label(format!("loaded {}", self.display.format(t, "%H:%M:%S")));
            }
            ui.separator();
            let end = a.minute_time(a.params.window_minutes);
            ui.label(format!(
                "window {} → {} ({})",
                self.display.format(a.params.window_start, "%a %d %b %H:%M"),
                self.display.format(end, "%a %d %b %H:%M"),
                self.display.label()
            ));
        });
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        let Some(a) = &self.analysis else { return };
        let heat = a.heat(self.metric);
        let unit = metric_unit(self.metric, self.window_days);

        // Peak minute in absolute time.
        let series = a.series(self.metric);
        if let Some((i, &peak)) = series.iter().enumerate().max_by_key(|(i, v)| (**v, std::cmp::Reverse(*i))) {
            ui.heading("Peak");
            ui.label(format!(
                "{} {} at {}",
                peak,
                if self.metric == Metric::Starts { "jobs start" } else { "jobs running" },
                self.display.format(a.minute_time(i), "%a %H:%M")
            ));
            let total: u64 = series.iter().map(|&v| v as u64).sum();
            let busy = series.iter().filter(|&&v| v > 0).count();
            ui.label(
                RichText::new(format!(
                    "{} of {} minutes have activity; average over those: {:.1}",
                    busy,
                    series.len(),
                    if busy == 0 { 0.0 } else { total as f64 / busy as f64 }
                ))
                .weak(),
            );
            ui.add_space(8.0);
        }

        ui.heading("Hotspots");
        ui.label(RichText::new(format!("Busiest times of day ({unit})")).weak());
        let max = heat.iter().copied().fold(0.0, f32::max);
        for (cell, v) in a.hotspots(&heat, 12) {
            let jobs = a.cell_jobs(self.metric, cell).len();
            let selected = self.selected_cell == Some(cell);
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(12.0, 12.0), Sense::hover());
                ui.painter().rect_filled(rect, 2.0, heat_color(v / max));
                let text = format!("{:02}:{:02}   {:>5.1}   {} jobs", cell / 60, cell % 60, v, jobs);
                if ui.selectable_label(selected, RichText::new(text).monospace()).clicked() {
                    self.selected_cell = Some(cell);
                    self.tab = Tab::Heatmap;
                }
            });
        }

        ui.add_space(8.0);
        ui.separator();
        if let Some(cell) = self.selected_cell {
            ui.heading(format!("{:02}:{:02}", cell / 60, cell % 60));
            ui.label(format!("{:.1} {}", heat[cell], unit));
            let mut list = a.cell_jobs(self.metric, cell).clone();
            list.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));
            ui.label(RichText::new(format!("{} jobs", list.len())).weak());
            egui::ScrollArea::vertical().id_salt("cell_jobs").show(ui, |ui| {
                for (j, _) in list {
                    let job = &self.jobs[j];
                    let r = ui.selectable_label(self.selected_job == Some(j), job.full_name());
                    if r.on_hover_text(format!("{}  {}", job.schedule, job.time_zone.as_deref().unwrap_or(""))).clicked() {
                        self.selected_job = Some(j);
                    }
                }
            });
        } else {
            ui.label(RichText::new("Click a cell in the heatmap or a hotspot to see which jobs fire then.").weak());
        }
    }

    fn heatmap(&mut self, ui: &mut egui::Ui) {
        let Some(a) = &self.analysis else { return };
        let heat = a.heat(self.metric);
        let max = heat.iter().copied().fold(0.0f32, f32::max).max(f32::EPSILON);
        let unit = metric_unit(self.metric, self.window_days);

        ui.horizontal(|ui| {
            ui.label(format!("Time of day ({}): rows are hours, columns are minutes. Colour = {unit}.", self.display.label()));
        });
        legend(ui, max);
        ui.add_space(4.0);

        let left = 34.0;
        let top = 16.0;
        let right = 46.0;
        let bottom_strip = 46.0;
        let avail = ui.available_size();
        let cell_w = ((avail.x - left - right) / 60.0).clamp(6.0, 30.0);
        let cell_h = ((avail.y - top - bottom_strip - 24.0) / 24.0).clamp(10.0, 28.0);
        let size = Vec2::new(left + cell_w * 60.0 + right, top + cell_h * 24.0 + bottom_strip);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        let painter = ui.painter_at(rect);
        let origin = rect.min + Vec2::new(left, top);
        let text_color = ui.visuals().text_color();
        let weak = ui.visuals().weak_text_color();
        let empty = ui.visuals().faint_bg_color;
        let font = FontId::monospace(10.0);

        let cell_rect = |cell: usize| {
            let (h, m) = (cell / 60, cell % 60);
            Rect::from_min_size(
                origin + Vec2::new(m as f32 * cell_w, h as f32 * cell_h),
                Vec2::new(cell_w, cell_h),
            )
        };

        for m in (0..60).step_by(5) {
            painter.text(
                origin + Vec2::new(m as f32 * cell_w + cell_w / 2.0, -3.0),
                Align2::CENTER_BOTTOM,
                format!(":{m:02}"),
                font.clone(),
                weak,
            );
        }
        let row_sums: Vec<f32> = (0..24).map(|h| heat[h * 60..h * 60 + 60].iter().sum()).collect();
        for h in 0..24 {
            let y = origin.y + h as f32 * cell_h + cell_h / 2.0;
            painter.text(Pos2::new(rect.min.x + left - 4.0, y), Align2::RIGHT_CENTER, format!("{h:02}"), font.clone(), weak);
            painter.text(
                Pos2::new(origin.x + 60.0 * cell_w + 4.0, y),
                Align2::LEFT_CENTER,
                format!("{:.0}", row_sums[h]),
                font.clone(),
                weak,
            );
        }

        for (cell, &v) in heat.iter().enumerate() {
            let r = cell_rect(cell).shrink(0.5);
            let color = if v <= 0.0 { empty } else { heat_color(v / max) };
            painter.rect_filled(r, 1.0, color);
        }

        // Cells where the selected job starts.
        if let Some(j) = self.selected_job
            && let Some(st) = a.jobs.get(j) {
                for &c in &st.start_cells {
                    painter.rect_stroke(cell_rect(c as usize).shrink(1.0), 1.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
                }
            }
        if let Some(c) = self.selected_cell {
            painter.rect_stroke(cell_rect(c).expand(1.0), 1.0, Stroke::new(2.0, text_color), StrokeKind::Outside);
        }

        // Bottom strip: totals per minute of the hour, which makes ":00 pile-ups" obvious.
        let col_sums: Vec<f32> = (0..60).map(|m| (0..24).map(|h| heat[h * 60 + m]).sum()).collect();
        let col_max = col_sums.iter().copied().fold(0.0f32, f32::max).max(f32::EPSILON);
        let strip_top = origin.y + 24.0 * cell_h + 6.0;
        let strip_h = bottom_strip - 24.0;
        for (m, &v) in col_sums.iter().enumerate() {
            let h = strip_h * v / col_max;
            let r = Rect::from_min_max(
                Pos2::new(origin.x + m as f32 * cell_w + 0.5, strip_top + strip_h - h),
                Pos2::new(origin.x + (m + 1) as f32 * cell_w - 0.5, strip_top + strip_h),
            );
            painter.rect_filled(r, 0.0, heat_color(v / col_max));
        }
        painter.text(
            Pos2::new(rect.min.x + left - 4.0, strip_top + strip_h / 2.0),
            Align2::RIGHT_CENTER,
            "Σ",
            font.clone(),
            weak,
        );
        painter.text(
            Pos2::new(origin.x, strip_top + strip_h + 3.0),
            Align2::LEFT_TOP,
            "per minute of the hour, summed over all hours",
            font.clone(),
            weak,
        );

        let hovered_cell = response.hover_pos().and_then(|p| {
            let d = p - origin;
            let (m, h) = ((d.x / cell_w).floor(), (d.y / cell_h).floor());
            (d.x >= 0.0 && d.y >= 0.0 && m < 60.0 && h < 24.0).then(|| h as usize * 60 + m as usize)
        });
        if let Some(cell) = hovered_cell {
            painter.rect_stroke(cell_rect(cell), 1.0, Stroke::new(1.0, text_color), StrokeKind::Inside);
            let jobs = a.cell_jobs(self.metric, cell);
            let names: Vec<String> = jobs.iter().take(8).map(|(j, _)| self.jobs[*j].full_name()).collect();
            let more = jobs.len().saturating_sub(names.len());
            let v = heat[cell];
            response.clone().on_hover_ui_at_pointer(|ui| {
                ui.strong(format!("{:02}:{:02} · {:.1} {}", cell / 60, cell % 60, v, unit));
                ui.label(format!("{} jobs", jobs.len()));
                for n in &names {
                    ui.label(RichText::new(n).monospace().small());
                }
                if more > 0 {
                    ui.label(RichText::new(format!("… and {more} more")).weak());
                }
            });
            if response.clicked() {
                self.selected_cell = Some(cell);
            }
        }
    }

    fn timeline(&mut self, ui: &mut egui::Ui) {
        let Some(a) = &self.analysis else { return };
        let bucket = if self.window_days == 1 { 1 } else { 10 };
        let series = a.series(self.metric);
        let bars: Vec<Bar> = series
            .chunks(bucket)
            .enumerate()
            .filter_map(|(i, c)| {
                let v = match self.metric {
                    Metric::Starts => c.iter().sum::<u32>(),
                    Metric::Running => c.iter().copied().max().unwrap_or(0),
                };
                (v > 0).then(|| {
                    let x = (i * bucket) as f64 + bucket as f64 / 2.0;
                    Bar::new(x, v as f64).width(bucket as f64 * 0.9).fill(heat_color(0.75))
                })
            })
            .collect();

        ui.label(format!(
            "{} per {} across the window ({}). Scroll to zoom, drag to pan, double-click to reset.",
            if self.metric == Metric::Starts { "Job starts" } else { "Peak jobs running" },
            if bucket == 1 { "minute".to_string() } else { format!("{bucket} minutes") },
            self.display.label()
        ));
        let start = a.params.window_start;
        let display = self.display;
        let fmt = if self.window_days == 1 { "%H:%M" } else { "%a %H:%M" };
        let to_time = move |x: f64| start + Duration::minutes(x.floor() as i64);
        Plot::new("timeline")
            .allow_scroll(false)
            .include_y(0.0)
            .x_axis_formatter(move |mark, _| display.format(to_time(mark.value), fmt))
            .label_formatter(move |hover| {
                let p = match hover {
                    egui_plot::HoverPosition::NearDataPoint { position, .. } => position,
                    egui_plot::HoverPosition::Elsewhere { position } => position,
                };
                Some(format!("{}\n{:.0}", display.format(to_time(p.x), "%a %d %b %H:%M"), p.y))
            })
            .show(ui, |plot_ui| {
                plot_ui.bar_chart(BarChart::new("load", bars));
            });
    }

    fn jobs_table(&mut self, ui: &mut egui::Ui) {
        let Some(a) = &self.analysis else { return };
        ui.horizontal(|ui| {
            ui.label("Sort by");
            ui.selectable_value(&mut self.sort, SortKey::Name, "Name");
            ui.selectable_value(&mut self.sort, SortKey::Runs, "Runs");
            ui.selectable_value(&mut self.sort, SortKey::Next, "Next run");
            ui.selectable_value(&mut self.sort, SortKey::Duration, "Run time");
        });
        ui.add_space(4.0);

        let mut rows: Vec<usize> = (0..self.jobs.len()).filter(|&i| a.jobs[i].matches_filter).collect();
        match self.sort {
            SortKey::Name => {}
            SortKey::Runs => rows.sort_by_key(|&i| std::cmp::Reverse(a.jobs[i].runs)),
            SortKey::Next => rows.sort_by_key(|&i| a.jobs[i].next_run.unwrap_or(DateTime::<Utc>::MAX_UTC)),
            SortKey::Duration => rows.sort_by_key(|&i| std::cmp::Reverse(a.jobs[i].duration_min)),
        }

        let window = if self.window_days == 1 { "Runs / 24h" } else { "Runs / 7d" };
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            egui::Grid::new("jobs").striped(true).spacing([14.0, 4.0]).show(ui, |ui| {
                for h in ["Namespace", "Name", "Schedule", "Time zone", window, "Next run", "Run time", "Status"] {
                    ui.strong(h);
                }
                ui.end_row();
                for i in rows {
                    let job = &self.jobs[i];
                    let st = &a.jobs[i];
                    let selected = self.selected_job == Some(i);
                    ui.label(&job.namespace);
                    if ui.selectable_label(selected, &job.name).on_hover_text("Highlight its start times in the heatmap").clicked() {
                        self.selected_job = if selected { None } else { Some(i) };
                    }
                    ui.label(RichText::new(&job.schedule).monospace());
                    ui.label(job.time_zone.as_deref().unwrap_or("UTC (default)"));
                    ui.label(st.runs.to_string());
                    ui.label(st.next_run.map(|t| self.display.format(t, "%a %H:%M")).unwrap_or_default());
                    let dur = if st.duration_estimated {
                        format!("{} min", st.duration_min)
                    } else {
                        format!("{} min (assumed)", st.duration_min)
                    };
                    ui.label(dur);
                    match &job.parsed {
                        Err(e) => ui.colored_label(ui.visuals().error_fg_color, format!("invalid: {e}")),
                        Ok(_) if job.suspended => ui.colored_label(ui.visuals().warn_fg_color, "suspended"),
                        Ok(_) if job.active > 0 => ui.label(format!("{} active", job.active)),
                        Ok(_) => ui.label(""),
                    };
                    ui.end_row();
                }
            });
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_loading();
        self.ensure_analysis();

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui);
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Heatmap, "Heatmap");
                ui.selectable_value(&mut self.tab, Tab::Timeline, "Timeline");
                ui.selectable_value(&mut self.tab, Tab::Jobs, "Jobs");
            });
            ui.add_space(2.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::right("side").default_size(300.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.side_panel(ui));
        });
        egui::CentralPanel::default().show(ui, |ui| {
            if self.jobs.is_empty() {
                ui.centered_and_justified(|ui| {
                    if self.loading.is_some() {
                        ui.spinner();
                    } else {
                        ui.label("No cronjobs loaded. Pick a context and press Fetch, or load demo data.");
                    }
                });
                return;
            }
            match self.tab {
                Tab::Heatmap => self.heatmap(ui),
                Tab::Timeline => self.timeline(ui),
                Tab::Jobs => self.jobs_table(ui),
            }
        });
    }
}

fn now_minute() -> DateTime<Utc> {
    Utc::now().duration_trunc(Duration::minutes(1)).unwrap()
}

fn metric_unit(metric: Metric, days: usize) -> &'static str {
    match (metric, days) {
        (Metric::Starts, 1) => "job starts",
        (Metric::Starts, _) => "avg job starts per day",
        (Metric::Running, 1) => "jobs running",
        (Metric::Running, _) => "avg jobs running",
    }
}

/// Dark blue → teal → yellow → orange → red.
fn heat_color(t: f32) -> Color32 {
    const STOPS: [(f32, [u8; 3]); 5] = [
        (0.0, [38, 62, 110]),
        (0.3, [42, 140, 160]),
        (0.6, [232, 197, 71]),
        (0.8, [240, 138, 36]),
        (1.0, [215, 38, 61]),
    ];
    let t = t.clamp(0.0, 1.0).powf(0.6);
    for w in STOPS.windows(2) {
        let ((t0, c0), (t1, c1)) = (w[0], w[1]);
        if t <= t1 {
            let f = (t - t0) / (t1 - t0);
            let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * f).round() as u8;
            return Color32::from_rgb(mix(c0[0], c1[0]), mix(c0[1], c1[1]), mix(c0[2], c1[2]));
        }
    }
    let c = STOPS[4].1;
    Color32::from_rgb(c[0], c[1], c[2])
}

fn legend(ui: &mut egui::Ui, max: f32) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("0").small());
        let (rect, _) = ui.allocate_exact_size(Vec2::new(180.0, 10.0), Sense::hover());
        let n = 60;
        for i in 0..n {
            let x0 = rect.min.x + rect.width() * i as f32 / n as f32;
            let x1 = rect.min.x + rect.width() * (i + 1) as f32 / n as f32;
            ui.painter().rect_filled(
                Rect::from_min_max(Pos2::new(x0, rect.min.y), Pos2::new(x1 + 0.5, rect.max.y)),
                0.0,
                heat_color(i as f32 / (n - 1) as f32),
            );
        }
        ui.label(RichText::new(format!("{max:.1}")).small());
    });
}
