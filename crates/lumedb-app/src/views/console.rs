//! Console: SQL command line with text output, like a terminal client.

use eframe::egui::{self, Color32, Key, RichText};
use egui_phosphor::regular as icon;
use lumedb_core::{trf, QueryResult};

use super::{enter_schema, Cx, View};
use crate::app::{take, Loc};
use crate::theme;
use crate::worker::TaskResult;

pub struct ConsoleTab {
    loc: Loc,
    input: String,
    output: String,
    history: Vec<String>,
    hist_pos: usize,
    pending: Option<u64>,
}

impl ConsoleTab {
    pub fn new(loc: Loc) -> Self {
        Self { loc, input: String::new(), output: String::new(), history: Vec::new(), hist_pos: 0, pending: None }
    }
}

/// Bordered text table, in the style of command-line clients.
pub fn ascii_table(r: &QueryResult) -> String {
    let cells: Vec<Vec<String>> = r
        .rows
        .iter()
        .map(|row| row.iter().map(|v| v.display().map(|s| s.replace('\n', " ")).unwrap_or_else(|| "NULL".into())).collect())
        .collect();
    let mut widths: Vec<usize> = r.columns.iter().map(|c| c.chars().count()).collect();
    for row in &cells {
        for (i, c) in row.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count().min(60));
        }
    }
    let line: String = format!("+{}+", widths.iter().map(|w| "-".repeat(w + 2)).collect::<Vec<_>>().join("+"));
    let fmt = |vals: Vec<String>| {
        let parts: Vec<String> = vals
            .iter()
            .zip(&widths)
            .map(|(v, w)| {
                let v: String = v.chars().take(*w).collect();
                format!(" {v}{} ", " ".repeat(w - v.chars().count()))
            })
            .collect();
        format!("|{}|", parts.join("|"))
    };
    let mut out = vec![line.clone(), fmt(r.columns.clone()), line.clone()];
    for row in cells {
        out.push(fmt(row));
    }
    out.push(line);
    out.join("\n")
}

impl View for ConsoleTab {
    fn title(&self, cx: &Cx) -> String {
        trf!("Console - {name}", name = cx.conn_name(self.loc.conn))
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::TERMINAL_WINDOW, theme::GREEN)
    }
    fn conn(&self) -> Option<u64> {
        Some(self.loc.conn)
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let bg = if ui.visuals().dark_mode { Color32::from_rgb(0x12, 0x12, 0x12) } else { Color32::from_rgb(0x1E, 0x1E, 0x1E) };
        let fg = Color32::from_rgb(0xD4, 0xD4, 0xD4);
        egui::Frame::none().fill(bg).inner_margin(egui::Margin::same(8.0)).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            egui::ScrollArea::vertical().auto_shrink(false).stick_to_bottom(true).max_height(ui.available_height() - 30.0).show(ui, |ui| {
                ui.label(RichText::new(&self.output).monospace().color(fg));
            });
            ui.horizontal(|ui| {
                let prompt = format!("{}>", self.loc.schema);
                ui.label(RichText::new(prompt).monospace().color(theme::GREEN));
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.input)
                        .font(egui::TextStyle::Monospace)
                        .text_color(fg)
                        .frame(false)
                        .desired_width(f32::INFINITY),
                );
                if self.pending.is_none() {
                    r.request_focus();
                }
                let (up, down) = ui.input(|i| (i.key_pressed(Key::ArrowUp), i.key_pressed(Key::ArrowDown)));
                if up && self.hist_pos > 0 {
                    self.hist_pos -= 1;
                    self.input = self.history[self.hist_pos].clone();
                }
                if down && self.hist_pos + 1 < self.history.len() {
                    self.hist_pos += 1;
                    self.input = self.history[self.hist_pos].clone();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !self.input.trim().is_empty() && self.pending.is_none() {
                    let sql = std::mem::take(&mut self.input);
                    self.output.push_str(&format!("{}> {sql}\n", self.loc.schema));
                    self.history.push(sql.clone());
                    self.hist_pos = self.history.len();
                    if sql.trim().eq_ignore_ascii_case("clear") || sql.trim().eq_ignore_ascii_case("cls") {
                        self.output.clear();
                        return;
                    }
                    let schema = self.loc.schema.clone();
                    self.pending = Some(cx.run(&self.loc, move |d, _| {
                        let _ = enter_schema(d, &schema);
                        let start = std::time::Instant::now();
                        let mut out = String::new();
                        for s in lumedb_core::sql::split_statements(&sql, d.kind()) {
                            match d.execute(&s.sql) {
                                Ok(r) if r.has_rows() => out.push_str(&format!("{}\n{}\n", ascii_table(&r), trf!("{n} rows", n = r.rows.len()))),
                                Ok(r) => out.push_str(&format!("{}\n", trf!("Query OK, {n} rows affected", n = r.affected.unwrap_or(0)))),
                                Err(e) => out.push_str(&format!("ERROR: {e:#}\n")),
                            }
                        }
                        out.push_str(&format!("({:.3} s)\n\n", start.elapsed().as_secs_f64()));
                        Ok(out)
                    }));
                }
            });
        });
    }

    fn on_result(&mut self, id: u64, res: TaskResult, _cx: &mut Cx) {
        if self.pending == Some(id) {
            self.pending = None;
            match take::<String>(res) {
                Ok(s) => self.output.push_str(&s),
                Err(e) => self.output.push_str(&format!("ERROR: {e}\n\n")),
            }
        }
    }
}
