//! Visual query builder: tables on a canvas, joins by dragging columns,
//! and below, fields, conditions, grouping, sorting and limit.

use std::collections::{BTreeMap, HashMap};

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use egui_phosphor::regular as icon;
use lumedb_core::{meta, tr, DbKind, ObjectKind};

use super::Cx;
use crate::app::{take, Loc};
use crate::theme;
use crate::widgets::highlight;
use crate::worker::TaskResult;

const BOX_W: f32 = 190.0;
const ROW_H: f32 = 20.0;
const HEAD_H: f32 = 24.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JoinKind {
    Inner,
    Left,
    Right,
    Full,
}

impl JoinKind {
    const ALL: [JoinKind; 4] = [JoinKind::Inner, JoinKind::Left, JoinKind::Right, JoinKind::Full];
    fn sql(self) -> &'static str {
        match self {
            JoinKind::Inner => "INNER JOIN",
            JoinKind::Left => "LEFT JOIN",
            JoinKind::Right => "RIGHT JOIN",
            JoinKind::Full => "FULL JOIN",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Agg {
    None,
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

impl Agg {
    const ALL: [Agg; 6] = [Agg::None, Agg::Count, Agg::Sum, Agg::Avg, Agg::Min, Agg::Max];
    fn label(self) -> &'static str {
        match self {
            Agg::None => tr("(no function)"),
            Agg::Count => "COUNT",
            Agg::Sum => "SUM",
            Agg::Avg => "AVG",
            Agg::Min => "MIN",
            Agg::Max => "MAX",
        }
    }
}

struct BTable {
    alias: String,
    table: String,
    pos: Pos2,
    cols: Vec<(String, bool)>,
}

/// Column of a canvas table: (alias, column).
type ColRef = (String, String);

struct Join {
    a: ColRef,
    b: ColRef,
    kind: JoinKind,
}

#[derive(Clone)]
struct Cond {
    field: String,
    op: String,
    value: String,
    or: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Fields,
    Where,
    Group,
    Order,
    Limit,
}

pub enum BuilderOutcome {
    Accept(String),
    Cancel,
}

pub struct QueryBuilder {
    loc: Loc,
    kind: DbKind,
    catalog: BTreeMap<String, Vec<String>>,
    fks: Vec<(String, String, String, String)>,
    tables: Vec<BTable>,
    joins: Vec<Join>,
    outputs: HashMap<ColRef, (String, Agg)>,
    conds: Vec<Cond>,
    orders: Vec<(String, bool)>,
    limit: String,
    offset: String,
    distinct: bool,
    auto_group: bool,
    pan: Vec2,
    dragging: Option<usize>,
    linking: Option<(ColRef, Pos2)>,
    filter: String,
    page: Page,
    load_id: Option<u64>,
}

impl QueryBuilder {
    pub fn new(loc: Loc, kind: DbKind) -> Self {
        Self {
            loc,
            kind,
            catalog: BTreeMap::new(),
            fks: Vec::new(),
            tables: Vec::new(),
            joins: Vec::new(),
            outputs: HashMap::new(),
            conds: Vec::new(),
            orders: Vec::new(),
            limit: String::new(),
            offset: String::new(),
            distinct: false,
            auto_group: true,
            pan: Vec2::new(16.0, 16.0),
            dragging: None,
            linking: None,
            filter: String::new(),
            page: Page::Fields,
            load_id: None,
        }
    }

    fn start(&mut self, cx: &mut Cx) {
        if self.load_id.is_some() || !self.catalog.is_empty() {
            return;
        }
        let schema = self.loc.schema.clone();
        self.load_id = Some(cx.run(&self.loc, move |d, _| {
            let mut cat = BTreeMap::new();
            let mut fks = Vec::new();
            for o in meta::list_objects(d, &schema)?.into_iter().filter(|o| matches!(o.kind, ObjectKind::Table | ObjectKind::View)) {
                cat.insert(o.name.clone(), meta::columns(d, &schema, &o.name).unwrap_or_default().into_iter().map(|c| c.name).collect::<Vec<_>>());
                if o.kind == ObjectKind::Table {
                    for f in meta::foreign_keys(d, &schema, &o.name).unwrap_or_default() {
                        for (c, rc) in f.columns.iter().zip(&f.ref_columns) {
                            fks.push((o.name.clone(), c.clone(), f.ref_table.clone(), rc.clone()));
                        }
                    }
                }
            }
            Ok((cat, fks))
        }));
    }

    pub fn on_result(&mut self, id: u64, res: TaskResult) -> bool {
        if self.load_id != Some(id) {
            return false;
        }
        self.load_id = None;
        type Loaded = (BTreeMap<String, Vec<String>>, Vec<(String, String, String, String)>);
        if let Ok((cat, fks)) = take::<Loaded>(res) {
            self.catalog = cat;
            self.fks = fks;
        }
        true
    }

    /// Adds a table to the canvas and joins it by foreign key to the ones already there.
    fn add_table(&mut self, table: &str, at: Option<Pos2>) {
        let mut alias = table.to_string();
        let mut n = 2;
        while self.tables.iter().any(|t| t.alias == alias) {
            alias = format!("{table}_{n}");
            n += 1;
        }
        let pos = at.unwrap_or_else(|| {
            let i = self.tables.len();
            Pos2::new((i % 4) as f32 * (BOX_W + 70.0), (i / 4) as f32 * 260.0)
        });
        let cols = self.catalog.get(table).cloned().unwrap_or_default().into_iter().map(|c| (c, false)).collect();
        for (t, c, rt, rc) in &self.fks {
            for other in &self.tables {
                if t == table && *rt == other.table {
                    self.joins.push(Join { a: (alias.clone(), c.clone()), b: (other.alias.clone(), rc.clone()), kind: JoinKind::Inner });
                } else if rt == table && *t == other.table {
                    self.joins.push(Join { a: (other.alias.clone(), c.clone()), b: (alias.clone(), rc.clone()), kind: JoinKind::Inner });
                }
            }
        }
        self.tables.push(BTable { alias, table: table.to_string(), pos, cols });
    }

    fn remove_table(&mut self, i: usize) {
        let alias = self.tables[i].alias.clone();
        self.joins.retain(|j| j.a.0 != alias && j.b.0 != alias);
        self.outputs.retain(|k, _| k.0 != alias);
        self.tables.remove(i);
    }

    fn col_sql(&self, c: &ColRef) -> String {
        format!("{}.{}", self.kind.quote_one(&c.0), self.kind.quote_one(&c.1))
    }

    fn field_names(&self) -> Vec<String> {
        self.tables.iter().flat_map(|t| t.cols.iter().map(move |(c, _)| format!("{}.{c}", t.alias))).collect()
    }

    fn field_sql(&self, f: &str) -> String {
        match f.split_once('.') {
            Some((a, c)) => self.col_sql(&(a.to_string(), c.to_string())),
            None => self.kind.quote_one(f),
        }
    }

    fn selected(&self) -> Vec<ColRef> {
        self.tables.iter().flat_map(|t| t.cols.iter().filter(|(_, on)| *on).map(move |(c, _)| (t.alias.clone(), c.clone()))).collect()
    }

    pub fn sql(&self) -> String {
        let q = |s: &str| self.kind.quote_one(s);
        if self.tables.is_empty() {
            return String::new();
        }
        let sel = self.selected();
        let mut fields: Vec<String> = Vec::new();
        let mut plain: Vec<String> = Vec::new();
        let mut any_agg = false;
        for c in &sel {
            let (alias, agg) = self.outputs.get(c).cloned().unwrap_or((String::new(), Agg::None));
            let base = self.col_sql(c);
            let expr = if agg == Agg::None {
                plain.push(base.clone());
                base
            } else {
                any_agg = true;
                format!("{}({base})", agg.label())
            };
            fields.push(if alias.trim().is_empty() { expr } else { format!("{expr} AS {}", q(alias.trim())) });
        }
        let distinct = if self.distinct { "DISTINCT " } else { "" };
        let mut sql = format!("SELECT {distinct}\n  {}", if fields.is_empty() { "*".into() } else { fields.join(",\n  ") });

        // FROM with the joins in connection order.
        let table_sql = |t: &BTable| if t.alias == t.table { q(&t.table) } else { format!("{} AS {}", q(&t.table), q(&t.alias)) };
        let mut placed: Vec<&str> = vec![&self.tables[0].alias];
        sql.push_str(&format!("\nFROM\n  {}", table_sql(&self.tables[0])));
        let mut used = vec![false; self.joins.len()];
        let mut remaining: Vec<&BTable> = self.tables.iter().skip(1).collect();
        while !remaining.is_empty() {
            let pick = remaining.iter().position(|t| {
                self.joins.iter().any(|j| (j.a.0 == t.alias && placed.contains(&j.b.0.as_str())) || (j.b.0 == t.alias && placed.contains(&j.a.0.as_str())))
            });
            let t = remaining.remove(pick.unwrap_or(0));
            let conds: Vec<(usize, &Join)> = self
                .joins
                .iter()
                .enumerate()
                .filter(|(i, j)| {
                    !used[*i] && ((j.a.0 == t.alias && placed.contains(&j.b.0.as_str())) || (j.b.0 == t.alias && placed.contains(&j.a.0.as_str())))
                })
                .collect();
            if conds.is_empty() {
                sql.push_str(&format!("\n  CROSS JOIN {}", table_sql(t)));
            } else {
                let kind = conds[0].1.kind;
                let on: Vec<String> = conds.iter().map(|(_, j)| format!("{} = {}", self.col_sql(&j.a), self.col_sql(&j.b))).collect();
                sql.push_str(&format!("\n  {} {} ON {}", kind.sql(), table_sql(t), on.join(" AND ")));
                for (i, _) in conds {
                    used[i] = true;
                }
            }
            placed.push(&t.alias);
        }

        let conds: Vec<String> = self
            .conds
            .iter()
            .filter(|c| !c.field.is_empty())
            .enumerate()
            .map(|(i, c)| {
                let lhs = self.field_sql(&c.field);
                let v = c.value.trim();
                let rhs = if c.op.starts_with("IS") {
                    String::new()
                } else if c.op.contains("IN") {
                    format!(" ({})", v.split(',').map(|x| lit(self.kind, x.trim())).collect::<Vec<_>>().join(", "))
                } else {
                    format!(" {}", lit(self.kind, v))
                };
                let join = if i == 0 {
                    ""
                } else if c.or {
                    "OR "
                } else {
                    "AND "
                };
                format!("{join}{lhs} {}{rhs}", c.op)
            })
            .collect();
        if !conds.is_empty() {
            sql.push_str(&format!("\nWHERE\n  {}", conds.join("\n  ")));
        }
        if any_agg && self.auto_group && !plain.is_empty() {
            sql.push_str(&format!("\nGROUP BY\n  {}", plain.join(",\n  ")));
        }
        if !self.orders.is_empty() {
            let o: Vec<String> = self
                .orders
                .iter()
                .filter(|(f, _)| !f.is_empty())
                .map(|(f, asc)| format!("{} {}", self.field_sql(f), if *asc { "ASC" } else { "DESC" }))
                .collect();
            if !o.is_empty() {
                sql.push_str(&format!("\nORDER BY\n  {}", o.join(",\n  ")));
            }
        }
        if let Ok(n) = self.limit.trim().parse::<u64>() {
            sql.push_str(&format!("\nLIMIT {n}"));
            if let Ok(o) = self.offset.trim().parse::<u64>() {
                sql.push_str(&format!(" OFFSET {o}"));
            }
        }
        sql.push(';');
        sql
    }

    pub fn ui(&mut self, ctx: &egui::Context, cx: &mut Cx) -> Option<BuilderOutcome> {
        self.start(cx);
        let mut outcome = None;
        let mut open = true;
        egui::Window::new(format!("{}  {}", icon::WRENCH, tr("Query Builder")))
            .id(egui::Id::new("query_builder_window"))
            .open(&mut open)
            .collapsible(false)
            .default_size([1100.0, 700.0])
            .default_pos(ctx.screen_rect().center() - Vec2::new(550.0, 350.0))
            .show(ctx, |ui| {
                if self.load_id.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr("Loading tables…"));
                    });
                    return;
                }
                egui::TopBottomPanel::bottom("qb_bottom").resizable(true).default_height(250.0).show_inside(ui, |ui| self.bottom_ui(ui, &mut outcome));
                egui::SidePanel::left("qb_tables").default_width(190.0).resizable(true).show_inside(ui, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text(format!("{}  {}", icon::MAGNIFYING_GLASS, tr("Search table")))
                            .desired_width(f32::INFINITY),
                    );
                    ui.label(RichText::new(tr("Double-click or drag onto the canvas")).size(11.0).color(theme::MUTED));
                    let f = self.filter.to_lowercase();
                    let names: Vec<String> = self.catalog.keys().filter(|t| f.is_empty() || t.to_lowercase().contains(&f)).cloned().collect();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for t in names {
                            let r = ui.add(egui::Label::new(format!("{}  {t}", icon::TABLE)).sense(Sense::click_and_drag()).selectable(false));
                            if r.double_clicked() {
                                self.add_table(&t, None);
                            }
                            if r.drag_started() {
                                ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("qb_drag_table"), t.clone()));
                            }
                        }
                    });
                });
                egui::CentralPanel::default().show_inside(ui, |ui| self.canvas_ui(ui));
            });
        if !open {
            outcome = Some(BuilderOutcome::Cancel);
        }
        outcome
    }

    fn canvas_ui(&mut self, ui: &mut egui::Ui) {
        let (rect, bg) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let dark = ui.visuals().dark_mode;
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, if dark { Color32::from_rgb(0x1A, 0x1B, 0x1E) } else { Color32::from_rgb(0xF8, 0xF9, 0xFB) });
        if self.tables.is_empty() {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                tr("Add tables from the list on the left"),
                egui::FontId::proportional(14.0),
                theme::MUTED,
            );
        }
        // Drop a table dragged from the list.
        let dragged: Option<String> = ui.ctx().data(|d| d.get_temp(egui::Id::new("qb_drag_table")));
        if let Some(t) = &dragged {
            if let Some(p) = ui.ctx().pointer_latest_pos() {
                painter.text(
                    p + Vec2::new(10.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    format!("{}  {t}", icon::TABLE),
                    egui::FontId::proportional(13.0),
                    theme::BLUE,
                );
            }
            if ui.input(|i| i.pointer.any_released()) {
                if let Some(p) = ui.ctx().pointer_latest_pos().filter(|p| rect.contains(*p)) {
                    let at = (p - rect.min - self.pan).to_pos2();
                    self.add_table(t, Some(at));
                }
                ui.ctx().data_mut(|d| d.remove::<String>(egui::Id::new("qb_drag_table")));
            }
        }
        if bg.dragged() && self.dragging.is_none() && self.linking.is_none() {
            self.pan += bg.drag_delta();
        }

        let origin = rect.min + self.pan;
        let mut row_rects: Vec<(ColRef, Rect)> = Vec::new();
        let mut remove = None;
        let mut start_link: Option<(ColRef, Pos2)> = None;
        let mut drop_link = false;
        let text = theme::text(ui.ctx());
        for i in 0..self.tables.len() {
            let t = &mut self.tables[i];
            let r = Rect::from_min_size(origin + t.pos.to_vec2(), Vec2::new(BOX_W, HEAD_H + ROW_H * (t.cols.len() + 1) as f32 + 6.0));
            if !r.intersects(rect) {
                continue;
            }
            painter.rect(r, 4.0, if dark { Color32::from_rgb(0x2B, 0x2D, 0x30) } else { Color32::WHITE }, Stroke::new(1.0, theme::border(ui.ctx())));
            let head = Rect::from_min_size(r.min, Vec2::new(BOX_W, HEAD_H));
            painter.rect_filled(head, egui::Rounding { nw: 4.0, ne: 4.0, sw: 0.0, se: 0.0 }, theme::kind_color(self.kind));
            let title = if t.alias == t.table { t.table.clone() } else { format!("{} ({})", t.table, t.alias) };
            painter.text(
                head.left_center() + Vec2::new(8.0, 0.0),
                egui::Align2::LEFT_CENTER,
                format!("{}  {title}", icon::TABLE),
                egui::FontId::proportional(13.0),
                Color32::WHITE,
            );
            let hr = ui.interact(head, ui.id().with(("qb_head", i)), Sense::click_and_drag());
            if hr.drag_started() {
                self.dragging = Some(i);
            }
            if hr.dragged() && self.dragging == Some(i) {
                t.pos += hr.drag_delta();
            }
            if hr.drag_stopped() {
                self.dragging = None;
            }
            let close = Rect::from_center_size(head.right_center() - Vec2::new(12.0, 0.0), Vec2::splat(16.0));
            painter.text(close.center(), egui::Align2::CENTER_CENTER, icon::X, egui::FontId::proportional(12.0), Color32::WHITE);
            if ui.interact(close, ui.id().with(("qb_close", i)), Sense::click()).clicked() {
                remove = Some(i);
            }
            // "*" row and columns with checkbox.
            let all_on = t.cols.iter().all(|(_, on)| *on) && !t.cols.is_empty();
            let mut rows: Vec<(String, bool)> = vec![("*".into(), all_on)];
            rows.extend(t.cols.iter().cloned());
            for (k, (c, on)) in rows.iter().enumerate() {
                let rr = Rect::from_min_size(r.min + Vec2::new(0.0, HEAD_H + 3.0 + ROW_H * k as f32), Vec2::new(BOX_W, ROW_H));
                let check = Rect::from_min_size(rr.min + Vec2::new(8.0, 3.0), Vec2::splat(14.0));
                painter.rect(check, 2.0, if *on { theme::BLUE } else { Color32::TRANSPARENT }, Stroke::new(1.0, if *on { theme::BLUE } else { theme::MUTED }));
                if *on {
                    painter.text(check.center(), egui::Align2::CENTER_CENTER, icon::CHECK, egui::FontId::proportional(10.0), Color32::WHITE);
                }
                painter.text(rr.left_center() + Vec2::new(30.0, 0.0), egui::Align2::LEFT_CENTER, c, egui::FontId::proportional(12.0), text);
                let cr = ui.interact(check.expand(2.0), ui.id().with(("qb_chk", i, k)), Sense::click());
                if cr.clicked() {
                    if k == 0 {
                        for (_, v) in &mut t.cols {
                            *v = !all_on;
                        }
                    } else {
                        t.cols[k - 1].1 = !t.cols[k - 1].1;
                    }
                }
                if k > 0 {
                    let name_rect = Rect::from_min_max(rr.min + Vec2::new(28.0, 0.0), rr.max);
                    let lr = ui.interact(name_rect, ui.id().with(("qb_col", i, k)), Sense::drag()).on_hover_text(tr("Drag onto another column to join"));
                    let col: ColRef = (t.alias.clone(), c.clone());
                    if lr.drag_started() {
                        start_link = Some((col.clone(), rr.right_center()));
                    }
                    if lr.drag_stopped() {
                        drop_link = true;
                    }
                    row_rects.push((col, rr));
                }
            }
        }
        if let Some(s) = start_link {
            self.linking = Some(s);
        }

        // Joins.
        let find = |c: &ColRef| row_rects.iter().find(|(k, _)| k == c).map(|(_, r)| *r);
        let mut join_menu: Option<usize> = None;
        for (ji, j) in self.joins.iter().enumerate() {
            let (Some(a), Some(b)) = (find(&j.a), find(&j.b)) else { continue };
            let (p1, p2) = if a.center().x < b.center().x { (a.right_center(), b.left_center()) } else { (a.left_center(), b.right_center()) };
            let mid = Pos2::new((p1.x + p2.x) / 2.0, (p1.y + p2.y) / 2.0);
            let stroke = Stroke::new(1.5, theme::ORANGE);
            painter.line_segment([p1, Pos2::new(mid.x, p1.y)], stroke);
            painter.line_segment([Pos2::new(mid.x, p1.y), Pos2::new(mid.x, p2.y)], stroke);
            painter.line_segment([Pos2::new(mid.x, p2.y), p2], stroke);
            let diamond = Rect::from_center_size(mid, Vec2::splat(14.0));
            painter.add(egui::Shape::convex_polygon(
                vec![mid - Vec2::new(0.0, 7.0), mid + Vec2::new(7.0, 0.0), mid + Vec2::new(0.0, 7.0), mid - Vec2::new(7.0, 0.0)],
                theme::bg(ui.ctx()),
                stroke,
            ));
            let tag = match j.kind {
                JoinKind::Inner => "=",
                JoinKind::Left => "⊐",
                JoinKind::Right => "⊏",
                JoinKind::Full => "⊔",
            };
            painter.text(mid, egui::Align2::CENTER_CENTER, tag, egui::FontId::proportional(10.0), theme::ORANGE);
            let jr = ui.interact(diamond, ui.id().with(("qb_join", ji)), Sense::click()).on_hover_text(format!(
                "{} ON {}.{} = {}.{}",
                j.kind.sql(),
                j.a.0,
                j.a.1,
                j.b.0,
                j.b.1
            ));
            if jr.clicked() || jr.secondary_clicked() {
                join_menu = Some(ji);
            }
        }
        if let Some(ji) = join_menu {
            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("qb_join_menu"), ji));
        }
        let menu: Option<usize> = ui.ctx().data(|d| d.get_temp(egui::Id::new("qb_join_menu")));
        if let Some(ji) = menu.filter(|ji| *ji < self.joins.len()) {
            let mut close = false;
            egui::Window::new(tr("Join"))
                .id(egui::Id::new("qb_join_window"))
                .collapsible(false)
                .resizable(false)
                .fixed_pos(ui.ctx().pointer_latest_pos().unwrap_or(rect.center()))
                .show(ui.ctx(), |ui| {
                    let j = &mut self.joins[ji];
                    ui.label(format!("{}.{} = {}.{}", j.a.0, j.a.1, j.b.0, j.b.1));
                    for k in JoinKind::ALL {
                        ui.radio_value(&mut j.kind, k, k.sql());
                    }
                    ui.horizontal(|ui| {
                        if ui.button(tr("OK")).clicked() {
                            close = true;
                        }
                        if ui.button(format!("{}  {}", icon::TRASH, tr("Remove join"))).clicked() {
                            self.joins.remove(ji);
                            close = true;
                        }
                    });
                });
            if close {
                ui.ctx().data_mut(|d| d.remove::<usize>(egui::Id::new("qb_join_menu")));
            }
        }

        // Join in progress (dragging from a column).
        if let Some((from, p)) = &self.linking {
            if let Some(ptr) = ui.ctx().pointer_latest_pos() {
                painter.line_segment([*p, ptr], Stroke::new(1.5, theme::BLUE));
            }
            if drop_link || ui.input(|i| i.pointer.any_released()) {
                let target =
                    ui.ctx().pointer_latest_pos().and_then(|ptr| row_rects.iter().find(|(c, r)| r.contains(ptr) && c.0 != from.0).map(|(c, _)| c.clone()));
                if let Some(to) = target {
                    self.joins.push(Join { a: from.clone(), b: to, kind: JoinKind::Inner });
                }
                self.linking = None;
            }
        }
        if let Some(i) = remove {
            self.remove_table(i);
        }
    }

    fn bottom_ui(&mut self, ui: &mut egui::Ui, outcome: &mut Option<BuilderOutcome>) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.page, Page::Fields, tr("Fields"));
            ui.selectable_value(&mut self.page, Page::Where, tr("Conditions"));
            ui.selectable_value(&mut self.page, Page::Group, lumedb_core::trc("query", "Group"));
            ui.selectable_value(&mut self.page, Page::Order, tr("Sort"));
            ui.selectable_value(&mut self.page, Page::Limit, tr("Limit"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(tr("Cancel")).clicked() {
                    *outcome = Some(BuilderOutcome::Cancel);
                }
                if ui.add_enabled(!self.tables.is_empty(), egui::Button::new(RichText::new(format!("{}  {}", icon::CHECK, tr("OK"))).strong())).clicked() {
                    *outcome = Some(BuilderOutcome::Accept(self.sql()));
                }
            });
        });
        ui.separator();
        let fields = self.field_names();
        ui.columns(2, |cols| {
            let ui = &mut cols[0];
            egui::ScrollArea::vertical().id_salt("qb_page").show(ui, |ui| match self.page {
                Page::Fields => {
                    let sel = self.selected();
                    if sel.is_empty() {
                        ui.label(RichText::new(tr("Check columns on the canvas (no columns = *).")).color(theme::MUTED));
                    }
                    egui::Grid::new("qb_fields").num_columns(3).striped(true).show(ui, |ui| {
                        ui.strong(tr("Field"));
                        ui.strong("Alias");
                        ui.strong(tr("Function"));
                        ui.end_row();
                        for c in sel {
                            let entry = self.outputs.entry(c.clone()).or_insert((String::new(), Agg::None));
                            ui.label(format!("{}.{}", c.0, c.1));
                            ui.add(egui::TextEdit::singleline(&mut entry.0).desired_width(120.0));
                            egui::ComboBox::from_id_salt(("qb_agg", &c)).selected_text(entry.1.label()).width(100.0).show_ui(ui, |ui| {
                                for a in Agg::ALL {
                                    ui.selectable_value(&mut entry.1, a, a.label());
                                }
                            });
                            ui.end_row();
                        }
                    });
                    ui.checkbox(&mut self.distinct, "DISTINCT");
                }
                Page::Where => {
                    let mut remove = None;
                    for (i, c) in self.conds.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            if i > 0 {
                                egui::ComboBox::from_id_salt(("qb_andor", i)).selected_text(if c.or { "OR" } else { "AND" }).width(55.0).show_ui(ui, |ui| {
                                    ui.selectable_value(&mut c.or, false, "AND");
                                    ui.selectable_value(&mut c.or, true, "OR");
                                });
                            } else {
                                ui.add_space(63.0);
                            }
                            egui::ComboBox::from_id_salt(("qb_cf", i)).selected_text(c.field.as_str()).width(150.0).show_ui(ui, |ui| {
                                for f in &fields {
                                    ui.selectable_value(&mut c.field, f.clone(), f);
                                }
                            });
                            egui::ComboBox::from_id_salt(("qb_co", i)).selected_text(c.op.as_str()).width(90.0).show_ui(ui, |ui| {
                                for o in ["=", "<>", "<", "<=", ">", ">=", "LIKE", "NOT LIKE", "IN", "NOT IN", "IS NULL", "IS NOT NULL"] {
                                    ui.selectable_value(&mut c.op, o.to_string(), o);
                                }
                            });
                            if !c.op.starts_with("IS") {
                                ui.add(egui::TextEdit::singleline(&mut c.value).desired_width(120.0).hint_text(if c.op.contains("IN") {
                                    "a, b, c"
                                } else {
                                    tr("value")
                                }));
                            }
                            if ui.small_button(icon::X).clicked() {
                                remove = Some(i);
                            }
                        });
                    }
                    if let Some(i) = remove {
                        self.conds.remove(i);
                    }
                    if ui.button(format!("{}  {}", icon::PLUS, tr("Add condition"))).clicked() {
                        self.conds.push(Cond { field: fields.first().cloned().unwrap_or_default(), op: "=".into(), value: String::new(), or: false });
                    }
                }
                Page::Group => {
                    ui.checkbox(&mut self.auto_group, tr("Automatically group by fields without a function"));
                    ui.label(RichText::new(tr("Choose a function (COUNT, SUM…) in \"Fields\" to group.")).color(theme::MUTED));
                }
                Page::Order => {
                    let mut remove = None;
                    for (i, (f, asc)) in self.orders.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt(("qb_of", i)).selected_text(f.as_str()).width(180.0).show_ui(ui, |ui| {
                                for x in &fields {
                                    ui.selectable_value(f, x.clone(), x);
                                }
                            });
                            ui.selectable_value(asc, true, "ASC");
                            ui.selectable_value(asc, false, "DESC");
                            if ui.small_button(icon::X).clicked() {
                                remove = Some(i);
                            }
                        });
                    }
                    if let Some(i) = remove {
                        self.orders.remove(i);
                    }
                    if ui.button(format!("{}  {}", icon::PLUS, tr("Add sort"))).clicked() {
                        self.orders.push((fields.first().cloned().unwrap_or_default(), true));
                    }
                }
                Page::Limit => {
                    egui::Grid::new("qb_limit").num_columns(2).show(ui, |ui| {
                        ui.label(tr("Limit:"));
                        ui.add(egui::TextEdit::singleline(&mut self.limit).desired_width(80.0).hint_text(tr("no limit")));
                        ui.end_row();
                        ui.label(tr("Offset:"));
                        ui.add(egui::TextEdit::singleline(&mut self.offset).desired_width(80.0).hint_text("0"));
                        ui.end_row();
                    });
                }
            });
            let ui = &mut cols[1];
            ui.label(RichText::new("SQL").strong());
            let sql = self.sql();
            let empty = format!("-- {}", tr("Add tables to the canvas"));
            egui::ScrollArea::both().id_salt("qb_sql").show(ui, |ui| {
                ui.label(highlight(ui, if sql.is_empty() { &empty } else { &sql }, self.kind));
            });
        });
    }
}

fn lit(kind: DbKind, v: &str) -> String {
    if v.parse::<f64>().is_ok() {
        v.to_string()
    } else {
        kind.quote_literal(v.trim_matches('\''))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_sql_runs() {
        let cfg = lumedb_core::ConnectionConfig { kind: DbKind::Sqlite, path: ":memory:".into(), ..Default::default() };
        let mut d = lumedb_core::connect(&cfg).unwrap();
        d.execute("CREATE TABLE clientes (id INTEGER PRIMARY KEY, ciudad TEXT)").unwrap();
        d.execute("CREATE TABLE pedidos (id INTEGER PRIMARY KEY, cliente_id INTEGER REFERENCES clientes(id), total REAL)").unwrap();
        d.execute("INSERT INTO clientes VALUES (1, 'Madrid'), (2, 'Bilbao')").unwrap();
        d.execute("INSERT INTO pedidos VALUES (1, 1, 10), (2, 1, 5), (3, 2, 7)").unwrap();

        let loc = Loc { conn: 1, db: String::new(), schema: "main".into() };
        let mut b = QueryBuilder::new(loc, DbKind::Sqlite);
        b.catalog.insert("clientes".into(), vec!["id".into(), "ciudad".into()]);
        b.catalog.insert("pedidos".into(), vec!["id".into(), "cliente_id".into(), "total".into()]);
        b.fks.push(("pedidos".into(), "cliente_id".into(), "clientes".into(), "id".into()));
        b.add_table("clientes", None);
        b.add_table("pedidos", None);
        assert_eq!(b.joins.len(), 1, "the foreign key creates the join");
        b.tables[0].cols[1].1 = true; // ciudad
        b.tables[1].cols[0].1 = true; // pedidos.id
        b.outputs.insert(("pedidos".into(), "id".into()), ("n".into(), Agg::Count));
        b.conds.push(Cond { field: "pedidos.total".into(), op: ">".into(), value: "1".into(), or: false });
        b.orders.push(("clientes.ciudad".into(), true));
        b.limit = "10".into();
        let sql = b.sql();
        assert!(sql.contains("INNER JOIN \"pedidos\" ON"), "{sql}");
        assert!(sql.contains("GROUP BY"), "{sql}");
        let r = d.execute(sql.trim_end_matches(';')).unwrap();
        assert_eq!(r.columns, ["ciudad", "n"]);
        assert_eq!(r.text_rows(), vec![vec![Some("Bilbao".into()), Some("1".into())], vec![Some("Madrid".into()), Some("2".into())]]);
    }
}
