//! Server monitor: processes, variables and status.

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use egui_phosphor::regular as icon;
use lumedb_core::{admin, tr, trf, QueryResult, Value};

use super::{Cx, View};
use crate::app::{take, Loc};
use crate::theme;
use crate::widgets::{results_grid, tool_button, toolbar, GridState};
use crate::worker::TaskResult;

#[derive(PartialEq, Clone, Copy)]
enum Page {
    Processes,
    Variables,
    Status,
}

pub struct MonitorTab {
    loc: Loc,
    page: Page,
    data: [Option<Result<QueryResult, String>>; 3],
    grids: [GridState; 3],
    pending: [Option<u64>; 3],
    filter: String,
    auto: bool,
    interval: u64,
    last: Option<Instant>,
    kill_id: Option<u64>,
}

impl MonitorTab {
    pub fn new(loc: Loc) -> Self {
        Self {
            loc,
            page: Page::Processes,
            data: [None, None, None],
            grids: Default::default(),
            pending: [None; 3],
            filter: String::new(),
            auto: true,
            interval: 5,
            last: None,
            kill_id: None,
        }
    }

    fn load(&mut self, cx: &mut Cx, p: Page) {
        let i = p as usize;
        if self.pending[i].is_some() {
            return;
        }
        self.pending[i] = Some(cx.run(&self.loc, move |d, _| {
            Ok(match p {
                Page::Processes => admin::processes(d),
                Page::Variables => admin::variables(d),
                Page::Status => admin::status(d),
            }
            .map_err(|e| format!("{e:#}")))
        }));
        if p == Page::Processes {
            self.last = Some(Instant::now());
        }
    }
}

impl View for MonitorTab {
    fn title(&self, cx: &Cx) -> String {
        trf!("Server Monitor - {name}", name = cx.conn_name(self.loc.conn))
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::GAUGE, theme::RED)
    }
    fn key(&self) -> Option<String> {
        Some(format!("monitor:{}", self.loc.conn))
    }
    fn conn(&self) -> Option<u64> {
        Some(self.loc.conn)
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let i = self.page as usize;
        if self.data[i].is_none() {
            self.load(cx, self.page);
        }
        if self.page == Page::Processes && self.auto && self.last.is_some_and(|t| t.elapsed() > Duration::from_secs(self.interval)) {
            self.load(cx, Page::Processes);
        }
        if self.auto {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
        toolbar(ui, |ui| {
            ui.selectable_value(&mut self.page, Page::Processes, format!("{}  {}", icon::LIST_BULLETS, tr("Process List")));
            ui.selectable_value(&mut self.page, Page::Variables, format!("{}  {}", icon::SLIDERS, tr("Variables")));
            ui.selectable_value(&mut self.page, Page::Status, format!("{}  {}", icon::CHART_LINE, tr("Status")));
            ui.separator();
            if tool_button(ui, true, icon::ARROW_CLOCKWISE, tr("Refresh"), theme::BLUE).clicked() {
                self.load(cx, self.page);
            }
            if self.page == Page::Processes {
                ui.checkbox(&mut self.auto, tr("Refresh every"));
                ui.add(egui::DragValue::new(&mut self.interval).range(1..=120).suffix(" s"));
                let sel = self.grids[0].selected.and_then(|(r, _)| match &self.data[0] {
                    Some(Ok(q)) => q.rows.get(r).and_then(|row| row.first()).and_then(Value::to_text),
                    _ => None,
                });
                if tool_button(ui, sel.is_some(), icon::PROHIBIT, tr("Kill Process"), theme::RED).clicked() {
                    if let Some(id) = sel {
                        let sql = admin::kill_sql(cx.kind(self.loc.conn), &id);
                        self.kill_id = Some(cx.run(&self.loc, move |d, _| d.execute(&sql).map(|_| ())));
                    }
                }
            } else {
                ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text(format!("{}  {}", icon::MAGNIFYING_GLASS, tr("Filter"))).desired_width(200.0));
            }
            if self.pending[self.page as usize].is_some() {
                ui.spinner();
            }
        });
        let i = self.page as usize;
        match &self.data[i] {
            Some(Ok(r)) => {
                if self.page != Page::Processes && !self.filter.is_empty() {
                    let f = self.filter.to_lowercase();
                    let filtered = QueryResult {
                        columns: r.columns.clone(),
                        rows: r.rows.iter().filter(|row| row.iter().any(|v| v.to_text().is_some_and(|t| t.to_lowercase().contains(&f)))).cloned().collect(),
                        ..Default::default()
                    };
                    let mut st = GridState::default();
                    results_grid(ui, ("monitor_f", i), &filtered, &mut st);
                } else {
                    results_grid(ui, ("monitor", i), r, &mut self.grids[i]);
                }
            }
            Some(Err(e)) => {
                crate::notice::error_banner(ui, e);
            }
            None => {
                ui.label(RichText::new(tr("Loading…")).color(theme::MUTED));
            }
        }
    }

    fn on_result(&mut self, id: u64, res: TaskResult, cx: &mut Cx) {
        if self.kill_id == Some(id) {
            self.kill_id = None;
            if let Err(e) = res {
                cx.error(tr("Could not kill the process"), e);
            }
            self.load(cx, Page::Processes);
            return;
        }
        for i in 0..3 {
            if self.pending[i] == Some(id) {
                self.pending[i] = None;
                let r = take::<Result<QueryResult, String>>(res).and_then(|r| r);
                let keep = self.grids[i].selected;
                self.grids[i] = GridState::default();
                self.grids[i].selected = keep;
                self.data[i] = Some(r);
                return;
            }
        }
    }
}
