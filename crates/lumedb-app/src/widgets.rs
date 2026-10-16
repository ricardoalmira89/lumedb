use std::sync::Arc;

use eframe::egui::{self, Align2, Color32, FontId, Response, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use lumedb_core::sql::{is_keyword, tokenize, TokKind};
use lumedb_core::{tr, DbKind, QueryResult, Value};

use crate::theme;

/// Large button on the top ribbon: icon on top and text below.
pub fn ribbon_button(ui: &mut Ui, icon: &str, label: &str, color: Color32, dropdown: bool) -> Response {
    let label_font = FontId::proportional(12.0);
    let text_w = ui.fonts(|f| f.layout_no_wrap(label.to_owned(), label_font.clone(), Color32::WHITE).size().x);
    let size = egui::vec2((text_w + 16.0).max(58.0), 58.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let enabled = ui.is_enabled();
        let p = ui.painter();
        if enabled && (resp.hovered() || resp.is_pointer_button_down_on()) {
            p.rect_filled(rect, 4.0, theme::hover(ui.ctx()));
        }
        let icon_color = if enabled { color } else { Color32::from_gray(0xB8) };
        let text_color = if enabled { theme::text(ui.ctx()) } else { theme::MUTED };
        p.text(rect.center_top() + egui::vec2(0.0, 20.0), Align2::CENTER_CENTER, icon, FontId::proportional(26.0), icon_color);
        p.text(rect.center_top() + egui::vec2(0.0, 46.0), Align2::CENTER_CENTER, label, label_font, text_color);
        if dropdown {
            p.text(rect.right_top() + egui::vec2(-8.0, 30.0), Align2::CENTER_CENTER, egui_phosphor::regular::CARET_DOWN, FontId::proportional(9.0), text_color);
        }
    }
    resp.on_disabled_hover_text(tr("Not available for this selection"))
}

pub fn ribbon_separator(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 52.0), Sense::hover());
    ui.painter().vline(rect.center().x, rect.y_range(), egui::Stroke::new(1.0, theme::border(ui.ctx())));
}

/// Flat toolbar button: colored icon and text.
pub fn tool_button(ui: &mut Ui, enabled: bool, icon: &str, label: &str, color: Color32) -> Response {
    use egui::text::{LayoutJob, TextFormat};
    let (icon_color, text_color) = if enabled { (color, theme::text(ui.ctx())) } else { (Color32::from_gray(0xB8), theme::MUTED) };
    let mut job = LayoutJob::default();
    let fmt = |size, color| TextFormat { font_id: FontId::proportional(size), color, valign: egui::Align::Center, ..Default::default() };
    job.append(icon, 0.0, fmt(15.0, icon_color));
    if !label.is_empty() {
        job.append(label, 5.0, fmt(13.0, text_color));
    }
    ui.add_enabled(enabled, egui::Button::new(job).frame(false))
}

/// Toolbar of a tab.
pub fn toolbar(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    egui::Frame::none()
        .fill(theme::ribbon(ui.ctx()))
        .inner_margin(egui::Margin::symmetric(6.0, 4.0))
        .stroke(egui::Stroke::new(1.0, theme::border(ui.ctx())))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // If it doesn't fit, it scrolls instead of widening the tab.
            egui::ScrollArea::horizontal().id_salt(ui.id().with("toolbar")).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(
                ui,
                |ui| {
                    ui.horizontal(add);
                },
            );
        });
}

/// Paints the right and bottom border of a cell, like a spreadsheet.
pub fn cell_grid(ui: &mut Ui, header: bool, fill: Option<Color32>) {
    let r = ui.max_rect().expand2(ui.spacing().item_spacing * 0.5);
    let p = ui.painter();
    if header {
        p.rect_filled(r, 0.0, theme::header(ui.ctx()));
    } else if let Some(f) = fill {
        p.rect_filled(r, 0.0, f);
    }
    let stroke = egui::Stroke::new(1.0, theme::grid(ui.ctx()));
    p.vline(r.right(), r.y_range(), stroke);
    p.hline(r.x_range(), r.bottom(), stroke);
    // Breathing room between the grid line and the text.
    ui.add_space(4.0);
}

/// Width of the row indicator column, the narrow first column of the grids.
pub const INDICATOR_W: f32 = 16.0;

/// Marks of the row indicator column, as in Navicat.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RowMark {
    None,
    Current,
    New,
    Deleted,
}

/// Row indicator cell: ▶ on the current row, * on new rows, ✕ on deleted ones.
pub fn row_indicator(ui: &mut Ui, mark: RowMark) {
    let r = ui.max_rect();
    let c = r.center();
    let color = theme::text(ui.ctx()).gamma_multiply(0.7);
    let p = ui.painter();
    match mark {
        RowMark::None => {}
        RowMark::Current => {
            let pts = vec![egui::pos2(c.x - 2.5, c.y - 4.0), egui::pos2(c.x + 3.0, c.y), egui::pos2(c.x - 2.5, c.y + 4.0)];
            p.add(egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE));
        }
        RowMark::New => {
            p.text(c, Align2::CENTER_CENTER, "*", FontId::proportional(13.0), color);
        }
        RowMark::Deleted => {
            p.text(c, Align2::CENTER_CENTER, "✕", FontId::proportional(10.0), theme::RED);
        }
    }
}

/// Value of a grid cell. Numbers are right-aligned, like in spreadsheets and Navicat.
pub fn value_label(ui: &mut Ui, v: &Value) {
    match v.display() {
        Some(s) if matches!(v, Value::Number(_)) => {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                ui.add(egui::Label::new(s).truncate().selectable(false));
            });
        }
        Some(s) => {
            let text = s.lines().next().unwrap_or("");
            let rich = if matches!(v, Value::Bytes(_)) { RichText::new(text).color(theme::MUTED) } else { RichText::new(text) };
            ui.add(egui::Label::new(rich).truncate().selectable(false));
        }
        None => {
            ui.add(egui::Label::new(RichText::new("(Null)").color(theme::MUTED).italics()).selectable(false));
        }
    }
}

/// State of a read-only grid (selection and local sorting).
#[derive(Default)]
pub struct GridState {
    pub selected: Option<(usize, usize)>,
    /// Whole rows selected from the row indicator column (display positions). Shift+click
    /// selects a range, Ctrl+click adds or removes a row.
    pub sel_rows: std::collections::BTreeSet<usize>,
    anchor: Option<usize>,
    pub sort: Option<(usize, bool)>,
    order: Vec<usize>,
    rows_seen: usize,
}

impl GridState {
    fn order(&mut self, r: &QueryResult) -> &[usize] {
        if self.order.len() != r.rows.len() || self.rows_seen != r.rows.len() {
            self.order = (0..r.rows.len()).collect();
            self.rows_seen = r.rows.len();
            self.apply_sort(r);
        }
        &self.order
    }

    /// Click on a row (indicator column, or a cell with Shift/Ctrl) at display position `pos`.
    fn click_row(&mut self, pos: usize, mods: egui::Modifiers) {
        match self.anchor {
            Some(a) if mods.shift => {
                if !mods.command {
                    self.sel_rows.clear();
                }
                self.sel_rows.extend(a.min(pos)..=a.max(pos));
            }
            _ if mods.command => {
                if !self.sel_rows.remove(&pos) {
                    self.sel_rows.insert(pos);
                }
                self.anchor = Some(pos);
            }
            _ => {
                self.sel_rows = std::collections::BTreeSet::from([pos]);
                self.anchor = Some(pos);
            }
        }
    }

    fn apply_sort(&mut self, r: &QueryResult) {
        self.sel_rows.clear();
        self.anchor = None;
        self.order = (0..r.rows.len()).collect();
        if let Some((c, asc)) = self.sort {
            self.order.sort_by(|&a, &b| {
                let (x, y) = (&r.rows[a][c], &r.rows[b][c]);
                let ord = match (x.to_text().and_then(|v| v.parse::<f64>().ok()), y.to_text().and_then(|v| v.parse::<f64>().ok())) {
                    (Some(p), Some(q)) => p.partial_cmp(&q).unwrap_or(std::cmp::Ordering::Equal),
                    _ => x.to_text().cmp(&y.to_text()),
                };
                if asc {
                    ord
                } else {
                    ord.reverse()
                }
            });
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Read-only results grid, with per-column sorting and copy.
pub fn results_grid(ui: &mut Ui, id: impl std::hash::Hash, r: &QueryResult, st: &mut GridState) {
    let order: Vec<usize> = st.order(r).to_vec();
    let mut new_sort = None;
    ui.push_id(id, |ui| {
        // Solid scroll bars (with their own space, as in Navicat): floating ones cover the last
        // row and take the clicks meant for its cells.
        ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
        egui::ScrollArea::horizontal().auto_shrink(false).drag_to_scroll(false).show(ui, |ui| {
            let sel_fill = ui.visuals().selection.bg_fill;
            let row_fill = theme::row_selected(ui.ctx());
            TableBuilder::new(ui)
                .striped(false)
                // Dragging the content to scroll would take the presses that select and edit cells.
                .drag_to_scroll(false)
                .resizable(true)
                .auto_shrink(false)
                .sense(Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::exact(crate::widgets::INDICATOR_W))
                .columns(Column::initial(140.0).at_least(40.0).clip(true), r.columns.len())
                .header(22.0, |mut h| {
                    h.col(|ui| cell_grid(ui, true, None));
                    for (ci, c) in r.columns.iter().enumerate() {
                        h.col(|ui| {
                            cell_grid(ui, true, None);
                            let arrow = match st.sort {
                                Some((sc, true)) if sc == ci => " ▲",
                                Some((sc, false)) if sc == ci => " ▼",
                                _ => "",
                            };
                            let resp = ui.add(egui::Label::new(RichText::new(format!("{c}{arrow}")).strong()).sense(Sense::click()).selectable(false));
                            if resp.clicked() {
                                new_sort = Some(match st.sort {
                                    Some((sc, true)) if sc == ci => Some((ci, false)),
                                    Some((sc, false)) if sc == ci => None,
                                    _ => Some((ci, true)),
                                });
                            }
                        });
                    }
                })
                .body(|body| {
                    body.rows(21.0, order.len(), |mut row| {
                        let pos = row.index();
                        let i = order[pos];
                        let in_rows = st.sel_rows.contains(&pos);
                        let (_, indicator) = row.col(|ui| {
                            cell_grid(ui, true, None);
                            let current = st.selected.is_some_and(|(r, _)| r == i);
                            row_indicator(ui, if current { RowMark::Current } else { RowMark::None });
                        });
                        if indicator.clicked() {
                            st.click_row(pos, indicator.ctx.input(|i| i.modifiers));
                            st.selected = Some((i, 0));
                        }
                        for (ci, v) in r.rows[i].iter().enumerate() {
                            let selected = st.selected == Some((i, ci));
                            let in_row = st.selected.is_some_and(|(r, _)| r == i);
                            let fill = if selected || in_rows {
                                Some(sel_fill)
                            } else if in_row {
                                Some(row_fill)
                            } else {
                                None
                            };
                            let (_, resp) = row.col(|ui| {
                                cell_grid(ui, false, fill);
                                value_label(ui, v);
                            });
                            if resp.clicked() {
                                let mods = resp.ctx.input(|i| i.modifiers);
                                if mods.shift || mods.command {
                                    st.click_row(pos, mods);
                                } else {
                                    st.sel_rows.clear();
                                    st.anchor = Some(pos);
                                }
                                st.selected = Some((i, ci));
                            }
                            resp.context_menu(|ui| {
                                st.selected = Some((i, ci));
                                if ui.button(tr("Copy")).clicked() {
                                    ui.ctx().copy_text(v.to_text().unwrap_or_default());
                                    ui.close_menu();
                                }
                                if ui.button(tr("Copy Row (tab-separated)")).clicked() {
                                    let line: Vec<String> = r.rows[i].iter().map(|v| v.to_text().unwrap_or_default()).collect();
                                    ui.ctx().copy_text(line.join("\t"));
                                    ui.close_menu();
                                }
                                if ui.button(tr("Copy All with Header")).clicked() {
                                    let mut out = r.columns.join("\t");
                                    for row in &r.rows {
                                        out.push('\n');
                                        out.push_str(&row.iter().map(|v| v.to_text().unwrap_or_default()).collect::<Vec<_>>().join("\t"));
                                    }
                                    ui.ctx().copy_text(out);
                                    ui.close_menu();
                                }
                            });
                        }
                    });
                });
        });
    });
    if let Some(s) = new_sort {
        st.sort = s;
        st.apply_sort(r);
    }
    // Ctrl+C with whole rows selected copies them as tab-separated text, with a header line.
    let copy = ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
    if copy && !st.sel_rows.is_empty() && ui.memory(|m| m.focused().is_none()) {
        let mut out = r.columns.join("\t");
        for &pos in &st.sel_rows {
            let Some(&i) = st.order.get(pos) else { continue };
            let cells: Vec<String> = r.rows[i].iter().map(|v| v.to_text().unwrap_or_else(|| "NULL".into())).collect();
            out.push('\n');
            out.push_str(&cells.join("\t"));
        }
        ui.ctx().copy_text(out);
    }
}

/// Highlights SQL: keywords, strings, numbers, comments and identifiers.
pub fn highlight(ui: &Ui, text: &str, kind: DbKind) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let dark = ui.visuals().dark_mode;
    let font = FontId::monospace(13.0);
    let base = ui.visuals().text_color();
    let kw = if dark { Color32::from_rgb(0x6C, 0xA8, 0xF0) } else { Color32::from_rgb(0x00, 0x33, 0xB3) };
    let st = if dark { Color32::from_rgb(0xD6, 0x9D, 0x85) } else { Color32::from_rgb(0xA3, 0x15, 0x15) };
    let num = if dark { Color32::from_rgb(0xB5, 0xCE, 0xA8) } else { Color32::from_rgb(0x09, 0x86, 0x58) };
    let com = if dark { Color32::from_rgb(0x6A, 0x99, 0x55) } else { Color32::from_rgb(0x70, 0x80, 0x70) };
    let ident = if dark { Color32::from_rgb(0x4E, 0xC9, 0xB0) } else { Color32::from_rgb(0x26, 0x7F, 0x99) };
    let mut job = LayoutJob::default();
    for t in tokenize(text, kind) {
        let s = &text[t.start..t.end];
        let color = match t.kind {
            TokKind::Comment => com,
            TokKind::Str => st,
            TokKind::Number => num,
            TokKind::QuotedIdent => ident,
            TokKind::Word if is_keyword(s) => kw,
            _ => base,
        };
        let italics = t.kind == TokKind::Comment;
        job.append(s, 0.0, TextFormat { font_id: font.clone(), color, italics, ..Default::default() });
    }
    job
}

pub fn sql_layouter(kind: DbKind) -> impl FnMut(&Ui, &str, f32) -> Arc<egui::Galley> {
    move |ui, text, wrap_width| {
        let mut job = highlight(ui, text, kind);
        job.wrap.max_width = wrap_width;
        ui.fonts(|f| f.layout_job(job))
    }
}
