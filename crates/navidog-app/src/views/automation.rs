//! Automation: batch jobs, their editor and the background scheduler.

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use egui_phosphor::regular as icon;
use navidog_core::automation::{self, Job, JobStep, Schedule};
use navidog_core::transfer::ExportFormat;
use navidog_core::{tr, trf, ConnectionConfig};

use super::{Cx, View};
use crate::theme;
use crate::worker::Background;

/// Runs scheduled jobs while NaviDog is open.
#[derive(Default)]
pub struct Scheduler {
    last_check: Option<Instant>,
    running: Option<(String, Background<Vec<String>>)>,
    message: Option<String>,
}

impl Scheduler {
    pub fn tick(&mut self, ctx: &egui::Context, configs: &[ConnectionConfig]) {
        if let Some((name, job)) = &mut self.running {
            if job.poll() {
                let result = job.result.take().unwrap();
                let text = match &result {
                    Ok(log) => trf!("OK ({n} steps)", n = log.len()),
                    Err(e) => trf!("ERROR: {error}", error = e.lines().last().unwrap_or("")),
                };
                if let Ok(mut jobs) = automation::load_jobs() {
                    if let Some(j) = jobs.iter_mut().find(|j| &j.name == name) {
                        j.last_result = match &result {
                            Ok(log) => log.join("\n"),
                            Err(e) => e.clone(),
                        };
                    }
                    let _ = automation::save_jobs(&jobs);
                }
                self.message = Some(trf!("Automation \"{name}\": {text}", name = name, text = text));
                self.running = None;
            }
            return;
        }
        if self.last_check.is_some_and(|t| t.elapsed() < Duration::from_secs(30)) {
            return;
        }
        self.last_check = Some(Instant::now());
        ctx.request_repaint_after(Duration::from_secs(30));
        let Ok(mut jobs) = automation::load_jobs() else { return };
        let now = chrono::Local::now();
        if let Some(j) = jobs.iter_mut().find(|j| j.schedule.due(j.last_run.as_deref(), now)) {
            j.last_run = Some(now.format("%Y-%m-%d %H:%M:%S").to_string());
            let job = j.clone();
            let _ = automation::save_jobs(&jobs);
            let configs = configs.to_vec();
            self.running = Some((job.name.clone(), Background::spawn(ctx, move |_| automation::run_job(&job, &configs))));
        }
    }

    pub fn last_message(&self) -> Option<&str> {
        self.message.as_deref()
    }
}

#[derive(Default)]
pub struct AutomationTab {
    jobs: Vec<Job>,
    selected: Option<usize>,
    loaded: bool,
    run: Option<Background<Vec<String>>>,
    log: String,
    dirty: bool,
}

const FORMATS: [ExportFormat; 8] = ExportFormat::ALL;

impl AutomationTab {
    fn save(&mut self, cx: &mut Cx) {
        match automation::save_jobs(&self.jobs) {
            Ok(()) => self.dirty = false,
            Err(e) => cx.error(tr("Could not save the jobs"), e.to_string()),
        }
    }

    fn step_ui(ui: &mut egui::Ui, i: usize, step: &mut JobStep, conns: &[String]) -> bool {
        let mut remove = false;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(trf!("Step {n}", n = i + 1)).strong());
                let label = match step {
                    JobStep::Query { .. } => tr("Query"),
                    JobStep::Backup { .. } => tr("Backup"),
                    JobStep::Export { .. } => tr("Export"),
                };
                ui.label(RichText::new(label).color(theme::BLUE));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button(icon::TRASH).on_hover_text(tr("Remove step")).clicked() {
                        remove = true;
                    }
                });
            });
            let (connection, database) = match step {
                JobStep::Query { connection, database, .. } | JobStep::Backup { connection, database, .. } | JobStep::Export { connection, database, .. } => {
                    (connection, database)
                }
            };
            ui.horizontal(|ui| {
                ui.label(tr("Connection:"));
                egui::ComboBox::from_id_salt(("job_conn", i)).selected_text(connection.as_str()).width(180.0).show_ui(ui, |ui| {
                    for c in conns {
                        ui.selectable_value(connection, c.clone(), c);
                    }
                });
                ui.label(tr("Database (PostgreSQL):"));
                ui.add(egui::TextEdit::singleline(database).desired_width(110.0));
            });
            match step {
                JobStep::Query { sql, .. } => {
                    ui.add(egui::TextEdit::multiline(sql).code_editor().desired_rows(4).desired_width(f32::INFINITY).hint_text(tr("SQL to run")));
                }
                JobStep::Backup { schema, .. } => {
                    ui.horizontal(|ui| {
                        ui.label(tr("Schema / database:"));
                        ui.add(egui::TextEdit::singleline(schema).desired_width(180.0));
                    });
                }
                JobStep::Export { sql, path, format, .. } => {
                    ui.add(egui::TextEdit::multiline(sql).code_editor().desired_rows(3).desired_width(f32::INFINITY).hint_text("SELECT …"));
                    ui.horizontal(|ui| {
                        ui.label(tr("Format:"));
                        egui::ComboBox::from_id_salt(("job_fmt", i)).selected_text(format.as_str()).show_ui(ui, |ui| {
                            for f in FORMATS {
                                ui.selectable_value(format, f.extension().to_string(), f.label());
                            }
                        });
                        ui.label(tr("File:"));
                        ui.add(egui::TextEdit::singleline(path).desired_width(260.0));
                        if ui.button(icon::FOLDER_OPEN).clicked() {
                            if let Some(p) = rfd::FileDialog::new().save_file() {
                                *path = p.display().to_string();
                            }
                        }
                    });
                }
            }
        });
        remove
    }
}

impl View for AutomationTab {
    fn title(&self, _: &Cx) -> String {
        format!("{}{}", if self.dirty { "*" } else { "" }, tr("Automation"))
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::ROBOT, theme::GREEN)
    }
    fn key(&self) -> Option<String> {
        Some("automation".into())
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        if !self.loaded {
            self.loaded = true;
            self.jobs = automation::load_jobs().unwrap_or_default();
        }
        let conns: Vec<String> = cx.sessions.iter().map(|s| s.cfg.name.clone()).collect();
        crate::widgets::toolbar(ui, |ui| {
            if crate::widgets::tool_button(ui, true, icon::PLUS, tr("New Job"), theme::GREEN).clicked() {
                self.jobs.push(Job { name: trf!("Job {n}", n = self.jobs.len() + 1), ..Default::default() });
                self.selected = Some(self.jobs.len() - 1);
                self.dirty = true;
            }
            if crate::widgets::tool_button(ui, self.dirty, icon::FLOPPY_DISK, tr("Save"), theme::BLUE).clicked() {
                self.save(cx);
            }
            if crate::widgets::tool_button(ui, self.selected.is_some(), icon::TRASH, tr("Delete Job"), theme::RED).clicked() {
                if let Some(i) = self.selected.take() {
                    self.jobs.remove(i);
                    self.dirty = true;
                }
            }
            let running = self.run.as_ref().is_some_and(|r| r.result.is_none());
            if crate::widgets::tool_button(ui, self.selected.is_some() && !running, icon::PLAY, tr("Run Now"), theme::GREEN).clicked() {
                if let Some(job) = self.selected.and_then(|i| self.jobs.get(i)).cloned() {
                    let configs = cx.configs();
                    self.log = trf!("Running \"{name}\"…", name = job.name);
                    self.run = Some(Background::spawn(&cx.egui, move |_| automation::run_job(&job, &configs)));
                }
            }
            if running {
                ui.spinner();
            }
        });
        if let Some(r) = &mut self.run {
            if r.poll() {
                self.log = match r.result.take().unwrap() {
                    Ok(l) => l.join("\n"),
                    Err(e) => e,
                };
                self.run = None;
            }
        }
        egui::SidePanel::left("jobs").default_width(220.0).show_inside(ui, |ui| {
            ui.strong(tr("Jobs"));
            ui.separator();
            for (i, j) in self.jobs.iter().enumerate() {
                let r = ui.selectable_label(self.selected == Some(i), format!("{}  {}", icon::ROBOT, j.name));
                if r.clicked() {
                    self.selected = Some(i);
                }
                ui.label(RichText::new(j.schedule.describe()).size(11.0).color(theme::MUTED));
            }
        });
        egui::CentralPanel::default().show_inside(ui, |ui| {
            let Some(i) = self.selected.filter(|i| *i < self.jobs.len()) else {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new(tr("Create or select a job.")).color(theme::MUTED));
                });
                return;
            };
            let job = &mut self.jobs[i];
            let mut changed = false;
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("job_head").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label(tr("Name:"));
                    changed |= ui.add(egui::TextEdit::singleline(&mut job.name).desired_width(260.0)).changed();
                    ui.end_row();
                    ui.label(tr("Schedule:"));
                    ui.horizontal(|ui| {
                        let mut mode = match job.schedule {
                            Schedule::Manual => 0,
                            Schedule::EveryMinutes(_) => 1,
                            Schedule::Daily { .. } => 2,
                        };
                        let before = mode;
                        ui.radio_value(&mut mode, 0, tr("Manual"));
                        ui.radio_value(&mut mode, 1, tr("Every"));
                        if let Schedule::EveryMinutes(m) = &mut job.schedule {
                            changed |= ui.add(egui::DragValue::new(m).range(1..=10_080).suffix(" min")).changed();
                        }
                        ui.radio_value(&mut mode, 2, tr("Daily at"));
                        if let Schedule::Daily { hour, minute } = &mut job.schedule {
                            changed |= ui.add(egui::DragValue::new(hour).range(0..=23)).changed();
                            ui.label(":");
                            changed |= ui.add(egui::DragValue::new(minute).range(0..=59)).changed();
                        }
                        if mode != before {
                            changed = true;
                            job.schedule = match mode {
                                1 => Schedule::EveryMinutes(60),
                                2 => Schedule::Daily { hour: 3, minute: 0 },
                                _ => Schedule::Manual,
                            };
                        }
                    });
                    ui.end_row();
                    ui.label("");
                    changed |= ui.checkbox(&mut job.continue_on_error, tr("Continue if a step fails")).changed();
                    ui.end_row();
                    ui.label(tr("Last run:"));
                    ui.label(job.last_run.clone().unwrap_or_else(|| tr("never").into()));
                    ui.end_row();
                });
                ui.label(
                    RichText::new(trf!(
                        "Scheduled jobs run while NaviDog is open. For cron: navidog --run-job \"{name}\"",
                        name = job.name
                    ))
                    .size(11.0)
                    .color(theme::MUTED),
                );
                ui.separator();
                let mut remove = None;
                for (s, step) in job.steps.iter_mut().enumerate() {
                    let before = step.clone();
                    if Self::step_ui(ui, s, step, &conns) {
                        remove = Some(s);
                    }
                    changed |= *step != before;
                }
                if let Some(s) = remove {
                    job.steps.remove(s);
                    changed = true;
                }
                let first = conns.first().cloned().unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label(tr("Add step:"));
                    if ui.button(format!("{}  {}", icon::FILE_SQL, tr("Query"))).clicked() {
                        job.steps.push(JobStep::Query { connection: first.clone(), database: String::new(), sql: String::new() });
                        changed = true;
                    }
                    if ui.button(format!("{}  {}", icon::ARCHIVE, tr("Backup"))).clicked() {
                        job.steps.push(JobStep::Backup { connection: first.clone(), database: String::new(), schema: String::new() });
                        changed = true;
                    }
                    if ui.button(format!("{}  {}", icon::UPLOAD_SIMPLE, tr("Export"))).clicked() {
                        job.steps.push(JobStep::Export { connection: first.clone(), database: String::new(), sql: String::new(), path: String::new(), format: "csv".into() });
                        changed = true;
                    }
                });
                ui.separator();
                ui.strong(tr("Log"));
                let log = if self.log.is_empty() { job.last_result.clone() } else { self.log.clone() };
                ui.label(RichText::new(if log.is_empty() { tr("(empty)").into() } else { log }).monospace().size(12.0));
            });
            if changed {
                self.dirty = true;
            }
        });
    }
}
