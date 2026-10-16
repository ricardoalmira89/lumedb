//! ER diagram canvas shared by the "Model" tab and the "ER Diagram" mode of Objects.
//!
//! - Layered layout by foreign keys (referenced tables on the left), ordered to reduce
//!   crossings, with each group of related tables laid out separately.
//! - Relations are routed with A* over the grid: they avoid the boxes, avoid unnecessary
//!   turns and never share a segment with other relations.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap};
use std::path::PathBuf;

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use egui_phosphor::regular as icon;
use lumedb_core::ddl::TableDesign;
use lumedb_core::{meta, store, tr, trf, DbKind, ObjectKind};

use crate::app::Loc;
use crate::theme;

const ROW_H: f32 = 17.5;
const HEAD_H: f32 = 26.0;
const MIN_W: f32 = 130.0;
const MAX_W: f32 = 260.0;
const PAD: f32 = 10.0;
/// Step of the fine grid (major lines every 5) and of relation routing.
const GRID: f32 = 12.0;
const TOOLBAR_H: f32 = 30.0;

fn default_header() -> Color32 {
    Color32::from_rgb(0x33, 0x5E, 0x86)
}

/// Colors that can be assigned to tables.
const PALETTE: [(&str, Color32); 8] = [
    ("Blue (default)", Color32::from_rgb(0x33, 0x5E, 0x86)),
    ("Green", Color32::from_rgb(0x3B, 0x8A, 0x4E)),
    ("Orange", Color32::from_rgb(0xD0, 0x7A, 0x1E)),
    ("Red", Color32::from_rgb(0xB8, 0x3F, 0x3A)),
    ("Purple", Color32::from_rgb(0x7B, 0x52, 0xAB)),
    ("Teal", Color32::from_rgb(0x16, 0x8B, 0x8F)),
    ("Pink", Color32::from_rgb(0xC2, 0x4A, 0x82)),
    ("Gray", Color32::from_rgb(0x6B, 0x72, 0x7B)),
];

/// What the user requests from the canvas.
pub enum ErEvent {
    Open(String),
    Design(String),
    NewTable,
    Refresh,
    /// Foreign key drawn with the "Relation" tool.
    NewRelation {
        table: String,
        column: String,
        ref_table: String,
        ref_column: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineStyle {
    /// Column-to-column curves, like Unreal wires.
    Curves,
    /// Right angles from edge to edge, without crossing (Navicat style).
    Orthogonal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Select,
    Pan,
    Relation,
}

enum Drag {
    Tables,
    Pan,
    Band(Pos2),
    Link { table: String, column: String, from: Pos2 },
}

pub struct ErCanvas {
    pub tables: Vec<TableDesign>,
    pub pos: BTreeMap<String, Pos2>,
    colors: BTreeMap<String, Color32>,
    pub pan: Vec2,
    pub zoom: f32,
    pub selected: BTreeSet<String>,
    /// There are unsaved position or color changes.
    pub moved: bool,
    widths: BTreeMap<String, f32>,
    tool: Tool,
    drag: Option<Drag>,
    fit_pending: bool,
    layout_pending: bool,
    /// Route of each relation (in model coordinates) and the position signature it was computed with.
    routes: Vec<Vec<Pos2>>,
    routes_key: u64,
    pub style: LineStyle,
    /// Table under the pointer right now, since when, and the one being highlighted (with delay).
    hover_target: Option<String>,
    hover_since: f64,
    active_hover: Option<String>,
    /// Last highlighted table (so its rows fade out on leave).
    last_hover: Option<String>,
}

impl Default for ErCanvas {
    fn default() -> Self {
        Self {
            tables: Vec::new(),
            pos: BTreeMap::new(),
            colors: BTreeMap::new(),
            pan: Vec2::new(20.0, 20.0),
            zoom: 1.0,
            selected: BTreeSet::new(),
            moved: false,
            widths: BTreeMap::new(),
            tool: Tool::Select,
            drag: None,
            fit_pending: false,
            layout_pending: false,
            routes: Vec::new(),
            routes_key: 0,
            style: LineStyle::Curves,
            hover_target: None,
            hover_since: 0.0,
            active_hover: None,
            last_hover: None,
        }
    }
}

/// File with the positions of a schema (shared by Model and Objects).
pub fn layout_path(conn_name: &str, loc: &Loc) -> Option<PathBuf> {
    let name: String =
        format!("{conn_name}_{}_{}", loc.db, loc.schema).chars().map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    store::models_dir().ok().map(|d| d.join(format!("{name}.json")))
}

/// Task that reads the design of every table in a schema.
pub fn load_designs(d: &mut dyn lumedb_core::Driver, schema: &str, report: &dyn Fn(String)) -> anyhow::Result<Vec<TableDesign>> {
    let mut out = Vec::new();
    for o in meta::list_objects(d, schema)?.into_iter().filter(|o| o.kind == ObjectKind::Table) {
        report(trf!("Reading {name}…", name = o.name));
        out.push(meta::table_design(d, schema, &o.name)?);
    }
    Ok(out)
}

/// Relation between two canvas tables: (child, parent).
/// The references (child → parent) among `members` without those that close a cycle, found as
/// back edges of a depth-first search.
fn acyclic(es: &[(usize, usize)], members: &[usize]) -> Vec<(usize, usize)> {
    let set: std::collections::HashSet<usize> = members.iter().copied().collect();
    let mut out: HashMap<usize, Vec<usize>> = HashMap::new();
    for &(c, p) in es {
        if set.contains(&c) && set.contains(&p) {
            out.entry(c).or_default().push(p);
        }
    }
    // 0 = not visited, 1 = on the current path, 2 = done.
    let mut state: HashMap<usize, u8> = HashMap::new();
    let mut back: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let mut sorted = members.to_vec();
    sorted.sort_unstable();
    for &start in &sorted {
        if state.get(&start).copied().unwrap_or(0) != 0 {
            continue;
        }
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        state.insert(start, 1);
        while let Some(&mut (v, ref mut i)) = stack.last_mut() {
            let next = out.get(&v).and_then(|ps| ps.get(*i)).copied();
            *i += 1;
            match next {
                Some(p) => match state.get(&p).copied().unwrap_or(0) {
                    0 => {
                        state.insert(p, 1);
                        stack.push((p, 0));
                    }
                    1 => {
                        back.insert((v, p));
                    }
                    _ => {}
                },
                None => {
                    state.insert(v, 2);
                    stack.pop();
                }
            }
        }
    }
    es.iter().copied().filter(|e| set.contains(&e.0) && set.contains(&e.1) && !back.contains(e)).collect()
}

fn edges(tables: &[TableDesign]) -> Vec<(usize, usize)> {
    let idx: HashMap<&str, usize> = tables.iter().enumerate().map(|(i, t)| (t.name.as_str(), i)).collect();
    let mut out = Vec::new();
    for (i, t) in tables.iter().enumerate() {
        for f in &t.foreign_keys {
            if let Some(&j) = idx.get(f.ref_table.as_str()) {
                if i != j {
                    out.push((i, j));
                }
            }
        }
    }
    out
}

impl ErCanvas {
    pub fn load_layout(&mut self, path: Option<PathBuf>) {
        let Some(text) = path.and_then(|p| std::fs::read_to_string(p).ok()) else { return };
        let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&text) else { return };
        for (k, v) in map {
            // New format {x, y, color} or old [x, y].
            let (x, y, color) = match &v {
                serde_json::Value::Array(a) => (a.first().and_then(|n| n.as_f64()), a.get(1).and_then(|n| n.as_f64()), None),
                serde_json::Value::Object(o) => {
                    (o.get("x").and_then(|n| n.as_f64()), o.get("y").and_then(|n| n.as_f64()), o.get("color").and_then(|c| c.as_str()))
                }
                _ => (None, None, None),
            };
            if let (Some(x), Some(y)) = (x, y) {
                self.pos.insert(k.clone(), Pos2::new(x as f32, y as f32));
            }
            if let Some(c) = color.and_then(|c| u32::from_str_radix(c.trim_start_matches('#'), 16).ok()) {
                self.colors.insert(k, Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8));
            }
        }
    }

    pub fn save_layout(&mut self, path: Option<PathBuf>) -> Result<(), String> {
        let p = path.ok_or(tr("no data folder"))?;
        let mut map = serde_json::Map::new();
        for (k, v) in &self.pos {
            let mut o = serde_json::Map::new();
            o.insert("x".into(), serde_json::json!(v.x));
            o.insert("y".into(), serde_json::json!(v.y));
            if let Some(c) = self.colors.get(k) {
                o.insert("color".into(), serde_json::json!(format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())));
            }
            map.insert(k.clone(), serde_json::Value::Object(o));
        }
        let text = serde_json::to_string_pretty(&map).map_err(|e| e.to_string())?;
        std::fs::write(&p, text).map_err(|e| e.to_string())?;
        self.moved = false;
        Ok(())
    }

    /// Replaces the tables; if there were no positions, lays them out.
    pub fn set_tables(&mut self, tables: Vec<TableDesign>) {
        let fresh = self.pos.is_empty();
        self.tables = tables;
        self.routes_key = 0;
        if fresh {
            self.layout_pending = true;
            self.fit_pending = true;
        }
    }

    pub fn auto_layout(&mut self) {
        self.layout_pending = true;
        self.fit_pending = true;
    }

    pub fn fit(&mut self) {
        self.fit_pending = true;
    }

    pub fn zoom_by(&mut self, f: f32) {
        self.zoom = (self.zoom * f).clamp(0.3, 3.0);
    }

    fn height(t: &TableDesign) -> f32 {
        HEAD_H + ROW_H * t.columns.len() as f32 + 8.0
    }

    fn width(&self, t: &TableDesign) -> f32 {
        self.widths.get(&t.name).copied().unwrap_or(200.0)
    }

    fn world_rect(&self, t: &TableDesign) -> Option<Rect> {
        self.pos.get(&t.name).map(|p| Rect::from_min_size(*p, Vec2::new(self.width(t), Self::height(t))))
    }

    fn bounds(&self) -> Option<Rect> {
        self.tables.iter().filter_map(|t| self.world_rect(t)).reduce(|a, b| a.union(b))
    }

    fn header(&self, name: &str) -> Color32 {
        self.colors.get(name).copied().unwrap_or_else(default_header)
    }

    // ------------------------------------------------------------ Layout

    /// Dependency layers within each connected group, ordered by barycenter.
    fn do_layout(&mut self) {
        let n = self.tables.len();
        if n == 0 {
            return;
        }
        let es = edges(&self.tables);
        let mut adj = vec![Vec::new(); n];
        for &(a, b) in &es {
            adj[a].push(b);
            adj[b].push(a);
        }
        // Connected groups.
        let mut comp = vec![usize::MAX; n];
        let mut comps: Vec<Vec<usize>> = Vec::new();
        for s in 0..n {
            if comp[s] != usize::MAX {
                continue;
            }
            let c = comps.len();
            let mut stack = vec![s];
            let mut members = Vec::new();
            comp[s] = c;
            while let Some(v) = stack.pop() {
                members.push(v);
                for &w in &adj[v] {
                    if comp[w] == usize::MAX {
                        comp[w] = c;
                        stack.push(w);
                    }
                }
            }
            comps.push(members);
        }
        let w = |i: usize| self.widths.get(&self.tables[i].name).copied().unwrap_or(220.0);
        let h = |i: usize| Self::height(&self.tables[i]);
        let gap_x = GRID * 13.0;
        let gap_y = GRID * 4.0;

        let mut blocks: Vec<(Vec<(usize, Pos2)>, Vec2)> = Vec::new();
        let mut isolated: Vec<usize> = Vec::new();
        for members in comps {
            if members.len() == 1 {
                isolated.push(members[0]);
                continue;
            }
            // References that close a cycle (e.g. departments → employees → positions →
            // departments) are left out of the layering: with them the longest path never ends and
            // the tables of the cycle are pushed dozens of empty columns away.
            let dag = acyclic(&es, &members);
            // Layer = longest path from the tables that reference nobody.
            let mut layer: HashMap<usize, usize> = members.iter().map(|&m| (m, 0)).collect();
            for _ in 0..members.len() {
                let mut changed = false;
                for &(child, parent) in &dag {
                    if layer.contains_key(&child) {
                        let want = layer[&parent] + 1;
                        if want > layer[&child] && want < members.len() {
                            layer.insert(child, want);
                            changed = true;
                        }
                    }
                }
                if !changed {
                    break;
                }
            }
            // Tables that reference nobody are placed just left of the nearest table that
            // uses them (otherwise they would all end up in the first column).
            for _ in 0..members.len() {
                let mut changed = false;
                for &m in &members {
                    let has_parent = dag.iter().any(|&(c, _)| c == m);
                    if has_parent {
                        continue;
                    }
                    let children: Vec<usize> = dag.iter().filter(|&&(_, p)| p == m).map(|&(c, _)| layer[&c]).collect();
                    if let Some(&min_child) = children.iter().min() {
                        let want = min_child.saturating_sub(1);
                        if want > layer[&m] {
                            layer.insert(m, want);
                            changed = true;
                        }
                    }
                }
                if !changed {
                    break;
                }
            }
            // Consecutive layer numbers, with no empty columns.
            let used: std::collections::BTreeSet<usize> = layer.values().copied().collect();
            let renumber: HashMap<usize, usize> = used.into_iter().enumerate().map(|(i, l)| (l, i)).collect();
            for v in layer.values_mut() {
                *v = renumber[v];
            }
            let depth = layer.values().copied().max().unwrap_or(0) + 1;
            let mut layers: Vec<Vec<usize>> = vec![Vec::new(); depth];
            let mut sorted = members.clone();
            sorted.sort_by(|a, b| self.tables[*a].name.cmp(&self.tables[*b].name));
            for m in sorted {
                layers[layer[&m]].push(m);
            }
            // Barycenter: several forward and backward passes to reduce crossings.
            let rank =
                |layers: &Vec<Vec<usize>>| -> HashMap<usize, f32> { layers.iter().flat_map(|l| l.iter().enumerate().map(|(i, &v)| (v, i as f32))).collect() };
            for pass in 0..8 {
                let r = rank(&layers);
                let order: Vec<usize> = if pass % 2 == 0 { (1..depth).collect() } else { (0..depth.saturating_sub(1)).rev().collect() };
                for li in order {
                    let neighbor_layer = if pass % 2 == 0 { li - 1 } else { li + 1 };
                    let mut keyed: Vec<(f32, usize)> = layers[li]
                        .iter()
                        .map(|&v| {
                            let ns: Vec<f32> = adj[v].iter().filter(|w| layer.get(w) == Some(&neighbor_layer)).map(|w| r[w]).collect();
                            let bc = if ns.is_empty() { r[&v] } else { ns.iter().sum::<f32>() / ns.len() as f32 };
                            (bc, v)
                        })
                        .collect();
                    keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                    layers[li] = keyed.into_iter().map(|(_, v)| v).collect();
                }
            }
            // Coordinates: one column per layer, vertically centered.
            let heights: Vec<f32> = layers.iter().map(|l| l.iter().map(|&v| h(v)).sum::<f32>() + gap_y * (l.len().saturating_sub(1)) as f32).collect();
            let tallest = heights.iter().copied().fold(0.0, f32::max);
            let mut x = 0.0;
            let mut placed = Vec::new();
            for (li, l) in layers.iter().enumerate() {
                let mut y = (tallest - heights[li]) / 2.0;
                let col_w = l.iter().map(|&v| w(v)).fold(0.0, f32::max);
                for &v in l {
                    placed.push((v, Pos2::new(x, y)));
                    y += h(v) + gap_y;
                }
                x += col_w + gap_x;
            }
            blocks.push((placed, Vec2::new(x - gap_x, tallest)));
        }
        // Large groups first, placed in rows.
        blocks.sort_by_key(|b| std::cmp::Reverse(b.0.len()));
        let max_row = 2600.0;
        let (mut cx, mut cy, mut row_h) = (0.0f32, 0.0f32, 0.0f32);
        for (placed, size) in blocks {
            if cx > 0.0 && cx + size.x > max_row {
                cx = 0.0;
                cy += row_h + gap_x;
                row_h = 0.0;
            }
            for (v, p) in placed {
                self.pos.insert(self.tables[v].name.clone(), Pos2::new(snap(cx + p.x), snap(cy + p.y)));
            }
            cx += size.x + gap_x;
            row_h = row_h.max(size.y);
        }
        // Standalone tables in a grid below.
        if !isolated.is_empty() {
            let top = if cx == 0.0 && cy == 0.0 { 0.0 } else { cy + row_h + gap_x };
            let per_row = 5usize;
            let mut y = top;
            for chunk in isolated.chunks(per_row) {
                let mut x = 0.0;
                let mut tall = 0.0f32;
                for &v in chunk {
                    self.pos.insert(self.tables[v].name.clone(), Pos2::new(snap(x), snap(y)));
                    x += w(v) + gap_y * 2.0;
                    tall = tall.max(h(v));
                }
                y += tall + gap_y * 2.0;
            }
        }
        self.moved = true;
        self.routes_key = 0;
    }

    // ------------------------------------------------------------ Relation routing

    /// Signature of what forces rerouting everything: tables, relations and widths.
    fn layout_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for t in &self.tables {
            t.name.hash(&mut h);
            for f in &t.foreign_keys {
                f.ref_table.hash(&mut h);
            }
            (self.width(t) as i32).hash(&mut h);
        }
        h.finish()
    }

    /// Relations affected by moving the selected tables: their own and the ones they cover.
    fn affected_edges(&self) -> BTreeSet<usize> {
        let es = edges(&self.tables);
        let moved: Vec<Rect> =
            self.tables.iter().filter(|t| self.selected.contains(&t.name)).filter_map(|t| self.world_rect(t)).map(|r| r.expand(GRID)).collect();
        let mut out = BTreeSet::new();
        for (k, &(a, b)) in es.iter().enumerate() {
            if self.selected.contains(&self.tables[a].name) || self.selected.contains(&self.tables[b].name) {
                out.insert(k);
                continue;
            }
            let crosses =
                self.routes.get(k).is_some_and(|r| r.windows(2).any(|w| moved.iter().any(|m| m.intersects(Rect::from_two_pos(w[0], w[1]).expand(0.5)))));
            if crosses {
                out.insert(k);
            }
        }
        out
    }

    /// A* over the grid for every relation.
    fn compute_routes(&mut self) {
        self.route_edges(None);
    }

    /// Routes the given relations (or all) with A*. The rest are kept as they are.
    fn route_edges(&mut self, which: Option<&BTreeSet<usize>>) {
        let es = edges(&self.tables);
        let rects: Vec<Option<Rect>> = self.tables.iter().map(|t| self.world_rect(t)).collect();
        let Some(bounds) = rects.iter().flatten().copied().reduce(|a, b| a.union(b)) else {
            self.routes.clear();
            return;
        };
        let origin = Pos2::new(((bounds.min.x / GRID).floor() - 6.0) * GRID, ((bounds.min.y / GRID).floor() - 6.0) * GRID);
        let cols = ((bounds.width() / GRID).ceil() as i32 + 12).max(1);
        let rows = ((bounds.height() / GRID).ceil() as i32 + 12).max(1);
        let cell_of = |p: Pos2| (((p.x - origin.x) / GRID).round() as i32, ((p.y - origin.y) / GRID).round() as i32);
        let center = |c: (i32, i32)| Pos2::new(origin.x + c.0 as f32 * GRID, origin.y + c.1 as f32 * GRID);
        let n = (cols * rows) as usize;
        let id = |c: (i32, i32)| (c.1 * cols + c.0) as usize;
        let inside = |c: (i32, i32)| c.0 >= 0 && c.1 >= 0 && c.0 < cols && c.1 < rows;
        // Cells occupied by each box (with half a step of margin).
        let mut owner = vec![usize::MAX; n];
        for (ti, r) in rects.iter().enumerate() {
            let Some(r) = r else { continue };
            let r = r.expand(GRID * 1.1);
            let (a, b) = (cell_of(r.min), cell_of(r.max));
            for y in a.1.max(0)..=b.1.min(rows - 1) {
                for x in a.0.max(0)..=b.0.min(cols - 1) {
                    let p = center((x, y));
                    if r.contains(p) {
                        owner[id((x, y))] = ti;
                    }
                }
            }
        }
        // Use of each cell by already routed relations: bit 1 = horizontal, bit 2 = vertical.
        let mut used = vec![0u8; n];
        const DIRS: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
        let mut routes = match which {
            Some(_) => {
                let mut r = self.routes.clone();
                r.resize(es.len(), Vec::new());
                r
            }
            None => vec![Vec::new(); es.len()],
        };
        // Relations that are not rerouted occupy their cells.
        if let Some(w) = which {
            for (k, r) in routes.iter().enumerate() {
                if w.contains(&k) {
                    continue;
                }
                for seg in r.windows(2) {
                    let bit = if (seg[0].y - seg[1].y).abs() < 0.5 { 1 } else { 2 };
                    let steps = ((seg[1] - seg[0]).length() / (GRID / 2.0)).ceil().max(1.0) as usize;
                    for i in 0..=steps {
                        let c = cell_of(seg[0] + (seg[1] - seg[0]) * (i as f32 / steps as f32));
                        if inside(c) {
                            used[id(c)] |= bit;
                        }
                    }
                }
            }
        }
        let mut order: Vec<usize> = match which {
            Some(w) => w.iter().copied().filter(|k| *k < es.len()).collect(),
            None => (0..es.len()).collect(),
        };
        let dist = |k: usize| {
            let (a, b) = es[k];
            match (rects[a], rects[b]) {
                (Some(x), Some(y)) => (x.center() - y.center()).length(),
                _ => 0.0,
            }
        };
        order.sort_by(|a, b| dist(*a).partial_cmp(&dist(*b)).unwrap_or(std::cmp::Ordering::Equal));
        for k in order {
            let (a, b) = es[k];
            let (Some(ra), Some(rb)) = (rects[a], rects[b]) else { continue };
            // Exits: free cells next to the source box, preferring the middle of each side.
            let ring = |r: Rect, t: usize| -> Vec<((i32, i32), u32)> {
                let mut out = Vec::new();
                let (lo, hi) = (cell_of(r.min), cell_of(r.max));
                // The box also occupies its margin: exits are a bit further out.
                for y in lo.1 - 4..=hi.1 + 4 {
                    for x in lo.0 - 4..=hi.0 + 4 {
                        let c = (x, y);
                        if !inside(c) || owner[id(c)] != usize::MAX {
                            continue;
                        }
                        let touches = DIRS.iter().any(|d| {
                            let nb = (x + d.0, y + d.1);
                            inside(nb) && owner[id(nb)] == t
                        });
                        // Only facing a side (not diagonal to a corner), so the line reaches the box.
                        let p = center(c);
                        let inner = r.shrink(GRID * 0.75);
                        let facing = inner.x_range().contains(p.x) || inner.y_range().contains(p.y);
                        if touches && facing {
                            let off = ((p.x - r.center().x).abs().min((p.y - r.center().y).abs()) / GRID) as u32;
                            out.push((c, off));
                        }
                    }
                }
                out
            };
            let starts = ring(ra, a);
            let start_cells: BTreeSet<usize> = starts.iter().map(|(c, _)| id(*c)).collect();
            let goals: BTreeSet<usize> = ring(rb, b).into_iter().map(|(c, _)| id(c)).collect();
            if starts.is_empty() || goals.is_empty() {
                continue;
            }
            let target = rb.center();
            let heur = |c: (i32, i32)| {
                let p = center(c);
                let dx = ((p.x - target.x).abs() - rb.width() / 2.0).max(0.0) / GRID;
                let dy = ((p.y - target.y).abs() - rb.height() / 2.0).max(0.0) / GRID;
                ((dx + dy) * 10.0) as u32
            };
            // State = cell × arrival direction (to penalize turns).
            let mut best = vec![u32::MAX; n * 5];
            let mut prev = vec![usize::MAX; n * 5];
            let mut heap = BinaryHeap::new();
            for (c, off) in &starts {
                let s = id(*c) * 5 + 4;
                let cost = off * 8;
                if cost < best[s] {
                    best[s] = cost;
                    heap.push(Reverse((cost + heur(*c), cost, *c, 4usize)));
                }
            }
            let mut found = None;
            while let Some(Reverse((_, g, c, dir))) = heap.pop() {
                let s = id(c) * 5 + dir;
                if g > best[s] {
                    continue;
                }
                if goals.contains(&id(c)) {
                    found = Some(s);
                    break;
                }
                for (di, d) in DIRS.iter().enumerate() {
                    let nb = (c.0 + d.0, c.1 + d.1);
                    if !inside(nb) || owner[id(nb)] != usize::MAX {
                        continue;
                    }
                    let horizontal = d.1 == 0;
                    let u = used[id(nb)];
                    let mut cost = 10;
                    if dir != 4 && dir != di {
                        cost += 40; // turn
                    }
                    // Sharing a segment with another relation is practically forbidden; crossing it, barely penalized.
                    if (horizontal && u & 1 != 0) || (!horizontal && u & 2 != 0) {
                        cost += 400;
                    } else if u != 0 {
                        cost += 15;
                    }
                    let hugging = DIRS.iter().any(|d2| {
                        let nn = (nb.0 + d2.0, nb.1 + d2.1);
                        inside(nn) && owner[id(nn)] != usize::MAX
                    });
                    if hugging && !goals.contains(&id(nb)) && !start_cells.contains(&id(nb)) {
                        cost += 12;
                    }
                    let ng = g + cost;
                    let ns = id(nb) * 5 + di;
                    if ng < best[ns] {
                        best[ns] = ng;
                        prev[ns] = s;
                        heap.push(Reverse((ng + heur(nb), ng, nb, di)));
                    }
                }
            }
            let Some(mut s) = found else { continue };
            let mut cells = Vec::new();
            loop {
                let cell = s / 5;
                cells.push(((cell as i32) % cols, (cell as i32) / cols));
                if prev[s] == usize::MAX {
                    break;
                }
                s = prev[s];
            }
            cells.reverse();
            for w in cells.windows(2) {
                let bit = if w[0].1 == w[1].1 { 1 } else { 2 };
                used[id(w[0])] |= bit;
                used[id(w[1])] |= bit;
            }
            // Points: edge of the source box, cells (corners only) and edge of the target box.
            let edge_point = |r: Rect, p: Pos2| {
                if p.x < r.left() {
                    Pos2::new(r.left(), p.y)
                } else if p.x > r.right() {
                    Pos2::new(r.right(), p.y)
                } else if p.y < r.top() {
                    Pos2::new(p.x, r.top())
                } else {
                    Pos2::new(p.x, r.bottom())
                }
            };
            let mut pts: Vec<Pos2> = cells.iter().map(|&c| center(c)).collect();
            pts.insert(0, edge_point(ra, pts[0]));
            let last = *pts.last().unwrap();
            pts.push(edge_point(rb, last));
            routes[k] = simplify(pts);
        }
        self.routes = routes;
    }

    // ------------------------------------------------------------ Drawing

    pub fn ui(&mut self, ui: &mut egui::Ui, _kind: DbKind) -> Option<ErEvent> {
        let mut event = None;
        for t in &self.tables {
            let title = ui.fonts(|f| f.layout_no_wrap(t.name.clone(), FontId::proportional(13.0), Color32::WHITE).size().x) + 2.0 * PAD;
            let w = t
                .columns
                .iter()
                .map(|c| {
                    ui.fonts(|f| f.layout_no_wrap(format!("{}: {}", c.name, c.data_type), FontId::proportional(12.0), Color32::WHITE).size().x) + 24.0 + PAD
                })
                .fold(title, f32::max)
                .clamp(MIN_W, MAX_W);
            self.widths.insert(t.name.clone(), (w / GRID).ceil() * GRID);
        }
        if self.layout_pending {
            self.layout_pending = false;
            self.do_layout();
        }
        if self.tables.iter().any(|t| !self.pos.contains_key(&t.name)) {
            let max_y = self.pos.values().map(|p| p.y).fold(0.0, f32::max);
            let missing: Vec<String> = self.tables.iter().filter(|t| !self.pos.contains_key(&t.name)).map(|t| t.name.clone()).collect();
            for (i, n) in missing.into_iter().enumerate() {
                self.pos.insert(n, Pos2::new(snap((i % 4) as f32 * 300.0), snap(max_y + 80.0 + (i / 4) as f32 * 240.0)));
            }
        }

        let full = ui.available_rect_before_wrap();
        let rect = Rect::from_min_max(full.min, Pos2::new(full.max.x, full.max.y - TOOLBAR_H));
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        if self.fit_pending {
            self.fit_pending = false;
            if let Some(b) = self.bounds() {
                let b = b.expand(30.0);
                self.zoom = (rect.width() / b.width()).min(rect.height() / b.height()).clamp(0.3, 1.0);
                self.pan = rect.center() - rect.min - b.center().to_vec2() * self.zoom;
            }
        }
        let key = self.layout_key();
        if self.style == LineStyle::Orthogonal && (key != self.routes_key || self.routes.len() != edges(&self.tables).len()) {
            self.compute_routes();
            self.routes_key = key;
        }

        let painter = ui.painter_at(rect);
        let dark = ui.visuals().dark_mode;
        painter.rect_filled(rect, 0.0, if dark { Color32::from_rgb(0x1E, 0x1F, 0x22) } else { Color32::WHITE });
        let z = self.zoom;
        let step = GRID * z;
        let (minor, major) = if dark {
            (Color32::from_rgb(0x27, 0x29, 0x2D), Color32::from_rgb(0x31, 0x34, 0x39))
        } else {
            (Color32::from_rgb(0xF0, 0xF2, 0xF5), Color32::from_rgb(0xDF, 0xE3, 0xE9))
        };
        if step >= 4.0 {
            let mut i = ((rect.left() - rect.min.x - self.pan.x) / step).floor() as i64;
            loop {
                let x = rect.min.x + self.pan.x + i as f32 * step;
                if x > rect.right() {
                    break;
                }
                painter.vline(x, rect.y_range(), Stroke::new(1.0, if i.rem_euclid(5) == 0 { major } else { minor }));
                i += 1;
            }
            let mut j = ((rect.top() - rect.min.y - self.pan.y) / step).floor() as i64;
            loop {
                let y = rect.min.y + self.pan.y + j as f32 * step;
                if y > rect.bottom() {
                    break;
                }
                painter.hline(rect.x_range(), y, Stroke::new(1.0, if j.rem_euclid(5) == 0 { major } else { minor }));
                j += 1;
            }
        }

        let origin = rect.min + self.pan;
        let to_screen = |p: Pos2| origin + p.to_vec2() * z;
        let boxes: BTreeMap<String, Rect> =
            self.tables.iter().filter_map(|t| self.world_rect(t).map(|r| (t.name.clone(), Rect::from_min_max(to_screen(r.min), to_screen(r.max))))).collect();

        // Table under the pointer, with an intent delay so sweeping the mouse past does not flicker.
        let raw_hover: Option<String> =
            if self.drag.is_none() { resp.hover_pos().and_then(|p| boxes.iter().rev().find(|(_, r)| r.contains(p)).map(|(n, _)| n.clone())) } else { None };
        let now = ui.input(|i| i.time);
        if raw_hover != self.hover_target {
            self.hover_target = raw_hover;
            self.hover_since = now;
        }
        let wait = if self.hover_target.is_some() { 0.12 } else { 0.22 };
        if self.hover_target != self.active_hover {
            if now - self.hover_since >= wait {
                self.active_hover = self.hover_target.clone();
                if self.active_hover.is_some() {
                    self.last_hover = self.active_hover.clone();
                }
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(wait - (now - self.hover_since) + 0.01));
            }
        }
        let active = self.active_hover.clone();
        let fade = 0.25;
        let ctx = ui.ctx().clone();
        let anim = |key: (&str, &str, &str), on: bool| ctx.animate_value_with_time(egui::Id::new(("er_anim", key)), if on { 1.0 } else { 0.0 }, fade);
        let dim = anim(("dim", "", ""), active.is_some());

        let accent = Color32::from_rgb(0x2D, 0x8C, 0xFF);
        let line_color = if dark { Color32::from_rgb(0x8A, 0x90, 0x99) } else { Color32::from_rgb(0x55, 0x5B, 0x63) };
        // Tables connected to the highlighted one and rows involved in its relations.
        let related = |center: &Option<String>| -> (BTreeSet<String>, BTreeSet<(String, String)>) {
            let mut linked = BTreeSet::new();
            let mut rows = BTreeSet::new();
            if let Some(h) = center {
                for t in &self.tables {
                    for f in &t.foreign_keys {
                        if t.name == *h || f.ref_table == *h {
                            linked.insert(t.name.clone());
                            linked.insert(f.ref_table.clone());
                            rows.extend(f.columns.iter().map(|c| (t.name.clone(), c.clone())));
                            rows.extend(f.ref_columns.iter().map(|c| (f.ref_table.clone(), c.clone())));
                        }
                    }
                }
            }
            (linked, rows)
        };
        let (linked, _) = related(&active);
        // Rows are painted using the last highlighted table so they fade out on leave.
        let (_, hot_rows) = related(&self.last_hover);
        let wire_stroke = |ta: &str, fk: &str, tb: &str| -> Stroke {
            let on = active.as_deref().is_some_and(|h| h == ta || h == tb);
            let e = anim(("edge", ta, fk), on);
            let sel = self.selected.contains(ta) || self.selected.contains(tb);
            let base = if sel { theme::ORANGE } else { line_color };
            let base_w = if sel { 2.2 } else { 1.4 };
            let color = mix(base, accent, e).gamma_multiply(1.0 - 0.75 * dim * (1.0 - e));
            Stroke::new(base_w + (2.6 - base_w) * e, color)
        };
        let row_y = |t: &TableDesign, r: &Rect, col: &str| -> f32 {
            match t.columns.iter().position(|c| c.name == col) {
                Some(i) => r.top() + (HEAD_H + 4.0 + ROW_H * i as f32 + ROW_H / 2.0) * z,
                None => r.top() + HEAD_H * z / 2.0,
            }
        };

        // Wire ends (painted on top of the tables).
        let mut ends: Vec<(Pos2, f32, Pos2, f32, Stroke)> = Vec::new();
        match self.style {
            LineStyle::Curves => {
                for t in &self.tables {
                    let Some(ra) = boxes.get(&t.name) else { continue };
                    for f in &t.foreign_keys {
                        let Some(rb) = boxes.get(&f.ref_table) else { continue };
                        let Some(target) = self.tables.iter().find(|x| x.name == f.ref_table) else { continue };
                        let ya = row_y(t, ra, f.columns.first().map(String::as_str).unwrap_or(""));
                        let yb = row_y(target, rb, f.ref_columns.first().map(String::as_str).unwrap_or(""));
                        let (p0, d0, p3, d3) = wire(*ra, ya, *rb, yb, t.name == f.ref_table);
                        let stroke = wire_stroke(&t.name, &f.name, &f.ref_table);
                        let k = ((p3.x - p0.x).abs() * 0.5).max(50.0 * z) + (p3.y - p0.y).abs() * 0.15;
                        let c1 = p0 + Vec2::new(d0 * k, 0.0);
                        let c2 = p3 + Vec2::new(d3 * k, 0.0);
                        painter.add(egui::epaint::CubicBezierShape::from_points_stroke([p0, c1, c2, p3], false, Color32::TRANSPARENT, stroke));
                        ends.push((p0, d0, p3, d3, stroke));
                    }
                }
            }
            LineStyle::Orthogonal => {
                for (k, &(a, b)) in edges(&self.tables).iter().enumerate() {
                    let (na, nb) = (&self.tables[a].name, &self.tables[b].name);
                    let stroke = wire_stroke(na, &format!("{k}"), nb);
                    let pts: Vec<Pos2> = match self.routes.get(k).filter(|r| r.len() >= 2) {
                        Some(r) => r.iter().map(|p| to_screen(*p)).collect(),
                        None => {
                            let (Some(ra), Some(rb)) = (boxes.get(na), boxes.get(nb)) else { continue };
                            elbow(*ra, *rb)
                        }
                    };
                    for w in pts.windows(2) {
                        painter.line_segment([w[0], w[1]], stroke);
                    }
                    if pts.len() >= 2 {
                        let n = pts.len();
                        let dir = |from: Pos2, to: Pos2| if (from.y - to.y).abs() < 0.5 { (to.x - from.x).signum() } else { 0.0 };
                        ends.push((pts[0], dir(pts[0], pts[1]), pts[n - 1], dir(pts[n - 1], pts[n - 2]), stroke));
                    }
                }
            }
        }

        // Tables.
        let name_color = if dark { Color32::from_rgb(0xE0, 0xE2, 0xE6) } else { Color32::from_rgb(0x1E, 0x1E, 0x1E) };
        let type_color = if dark { Color32::from_rgb(0x8E, 0x94, 0x9C) } else { Color32::from_rgb(0x9A, 0x9A, 0x9A) };
        let key_color = Color32::from_rgb(0xE8, 0xA3, 0x17);
        for t in &self.tables {
            let Some(r) = boxes.get(&t.name) else { continue };
            if !r.intersects(rect) {
                continue;
            }
            let selected = self.selected.contains(&t.name);
            let hf = anim(("node", &t.name, ""), active.as_deref() == Some(t.name.as_str()));
            let lf = anim(("link", &t.name, ""), linked.contains(&t.name));
            let focus = hf.max(lf);
            // Soft dimming of the rest of the diagram.
            let t_dim = dim * (1.0 - focus);
            let head_color = self.header(&t.name).gamma_multiply(1.0 - 0.45 * t_dim);
            let (base_border, base_w) = if selected { (theme::ORANGE, 2.5) } else { (self.header(&t.name), 1.8) };
            let border = mix(base_border, accent, hf.max(lf * 0.8));
            let width = base_w + (3.0 - base_w) * hf + (2.0 - base_w).max(0.0) * lf * (1.0 - hf);
            painter.rect(*r, 6.0 * z, if dark { Color32::from_rgb(0x2B, 0x2D, 0x30) } else { Color32::WHITE }, Stroke::new(width, border));
            let head = Rect::from_min_size(r.min, Vec2::new(r.width(), HEAD_H * z));
            painter.rect_filled(head, egui::Rounding { nw: 6.0 * z, ne: 6.0 * z, sw: 0.0, se: 0.0 }, head_color);
            for dx in [0.0, 0.6] {
                painter.text(head.center() + Vec2::new(dx, 0.0), Align2::CENTER_CENTER, &t.name, FontId::proportional(13.0 * z), Color32::WHITE);
            }
            let fk_cols: Vec<&String> = t.foreign_keys.iter().flat_map(|f| &f.columns).collect();
            let idx_cols: Vec<&String> = t.indexes.iter().flat_map(|i| &i.columns).collect();
            for (i, c) in t.columns.iter().enumerate() {
                let y = r.top() + (HEAD_H + 4.0 + ROW_H * i as f32 + ROW_H / 2.0) * z;
                if lf > 0.01 && hot_rows.contains(&(t.name.clone(), c.name.clone())) {
                    let row = Rect::from_center_size(Pos2::new(r.center().x, y), Vec2::new(r.width() - 4.0, ROW_H * z));
                    painter.rect_filled(row, 2.0, accent.gamma_multiply(0.16 * lf));
                }
                let icon_pos = Pos2::new(r.left() + 13.0 * z, y);
                if c.primary_key {
                    painter.text(icon_pos, Align2::CENTER_CENTER, icon::KEY, FontId::proportional(12.0 * z), key_color);
                } else if fk_cols.contains(&&c.name) || idx_cols.contains(&&c.name) {
                    painter.text(icon_pos, Align2::CENTER_CENTER, icon::DIAMOND, FontId::proportional(10.0 * z), type_color);
                }
                let mut job = egui::text::LayoutJob::default();
                let alpha = 1.0 - 0.55 * t_dim;
                job.append(
                    &c.name,
                    0.0,
                    egui::TextFormat { font_id: FontId::proportional(12.0 * z), color: name_color.gamma_multiply(alpha), ..Default::default() },
                );
                job.append(
                    &format!(": {}", c.data_type),
                    0.0,
                    egui::TextFormat { font_id: FontId::proportional(12.0 * z), color: type_color.gamma_multiply(alpha), ..Default::default() },
                );
                job.wrap = egui::text::TextWrapping::truncate_at_width(r.width() - 30.0 * z);
                let galley = ui.fonts(|f| f.layout_job(job));
                painter.galley(Pos2::new(r.left() + 24.0 * z, y - galley.size().y / 2.0), galley, name_color);
            }
        }

        // Dot at the source and arrow at the target, on top of the borders.
        for (p0, d0, p3, d3, stroke) in ends {
            let _ = d0;
            painter.circle_filled(p0, 3.5 * z.max(0.6), stroke.color);
            if d3 == 0.0 {
                // Vertical arrival (orthogonal style): no arrow.
                continue;
            }
            // Arrowhead pointing to the target table.
            let len = 10.0 * z.max(0.6);
            let half = 5.0 * z.max(0.6);
            let base = p3 + Vec2::new(d3 * len, 0.0);
            painter.add(egui::Shape::convex_polygon(vec![p3, base + Vec2::new(0.0, -half), base + Vec2::new(0.0, half)], stroke.color, Stroke::NONE));
        }
        if self.tables.is_empty() {
            painter.text(rect.center(), Align2::CENTER_CENTER, tr("No tables"), FontId::proportional(14.0), theme::MUTED);
        }

        // What is under the pointer: table and column.
        let columns: HashMap<String, Vec<String>> = self.tables.iter().map(|t| (t.name.clone(), t.columns.iter().map(|c| c.name.clone()).collect())).collect();
        let hit = |p: Pos2| -> Option<(String, Option<String>)> {
            let (name, r) = boxes.iter().rev().find(|(_, r)| r.contains(p))?;
            let row = ((p.y - r.top()) / z - HEAD_H - 4.0) / ROW_H;
            let col = (row >= 0.0).then(|| columns.get(name).and_then(|c| c.get(row as usize)).cloned()).flatten();
            Some((name.clone(), col))
        };

        let over_mini = self.minimap(ui, rect, origin, &painter);

        // Interaction depending on the tool.
        let middle = ui.input(|i| i.pointer.middle_down());
        let pointer = resp.interact_pointer_pos();
        if resp.drag_started() && !over_mini {
            let p = pointer.unwrap_or(rect.center());
            let under = hit(p);
            self.drag = Some(match (self.tool, &under) {
                _ if middle => Drag::Pan,
                (Tool::Pan, _) => Drag::Pan,
                (Tool::Relation, Some((t, Some(c)))) => Drag::Link { table: t.clone(), column: c.clone(), from: p },
                (Tool::Relation, _) => Drag::Pan,
                (Tool::Select, Some((t, _))) => {
                    if !self.selected.contains(t) {
                        if !ui.input(|i| i.modifiers.command || i.modifiers.shift) {
                            self.selected.clear();
                        }
                        self.selected.insert(t.clone());
                    }
                    Drag::Tables
                }
                (Tool::Select, None) => Drag::Band(p),
            });
        }
        if resp.dragged() {
            let delta = resp.drag_delta();
            match &self.drag {
                Some(Drag::Pan) => self.pan += delta,
                Some(Drag::Tables) => {
                    for n in &self.selected {
                        if let Some(p) = self.pos.get_mut(n) {
                            *p += delta / z;
                        }
                    }
                    self.moved = true;
                    if delta != Vec2::ZERO && self.style == LineStyle::Orthogonal {
                        let affected = self.affected_edges();
                        self.route_edges(Some(&affected));
                    }
                }
                Some(Drag::Band(start)) => {
                    if let Some(p) = ui.ctx().pointer_latest_pos() {
                        let band = Rect::from_two_pos(*start, p);
                        painter.rect(band, 0.0, theme::BLUE.gamma_multiply(0.12), Stroke::new(1.0, theme::BLUE));
                    }
                }
                Some(Drag::Link { from, .. }) => {
                    if let Some(p) = ui.ctx().pointer_latest_pos() {
                        painter.line_segment([*from, p], Stroke::new(2.0, theme::ORANGE));
                        painter.circle_filled(*from, 3.0, theme::ORANGE);
                    }
                }
                None => {}
            }
        }
        if resp.drag_stopped() {
            match self.drag.take() {
                Some(Drag::Tables) => {
                    for n in &self.selected {
                        if let Some(p) = self.pos.get_mut(n) {
                            *p = Pos2::new(snap(p.x), snap(p.y));
                        }
                    }
                    if self.style == LineStyle::Orthogonal {
                        let affected = self.affected_edges();
                        self.route_edges(Some(&affected));
                    }
                }
                Some(Drag::Band(start)) => {
                    if let Some(p) = ui.ctx().pointer_latest_pos() {
                        let band = Rect::from_two_pos(start, p);
                        if !ui.input(|i| i.modifiers.command || i.modifiers.shift) {
                            self.selected.clear();
                        }
                        self.selected.extend(boxes.iter().filter(|(_, r)| band.intersects(**r)).map(|(n, _)| n.clone()));
                    }
                }
                Some(Drag::Link { table, column, .. }) => {
                    if let Some((rt, Some(rc))) = ui.ctx().pointer_latest_pos().and_then(hit) {
                        if rt != table {
                            event = Some(ErEvent::NewRelation { table, column, ref_table: rt, ref_column: rc });
                        }
                    }
                }
                _ => {}
            }
        }
        if resp.clicked() && !over_mini && self.tool == Tool::Select {
            let under = pointer.and_then(hit).map(|(t, _)| t);
            if !ui.input(|i| i.modifiers.command || i.modifiers.shift) {
                self.selected.clear();
            }
            if let Some(t) = under {
                if !self.selected.remove(&t) {
                    self.selected.insert(t);
                }
            }
        }
        if resp.double_clicked() && !over_mini {
            if let Some((t, _)) = pointer.and_then(hit) {
                event = Some(ErEvent::Open(t));
            }
        }
        if resp.hovered() && !over_mini {
            let (scroll, ctrl) = ui.input(|i| (i.raw_scroll_delta, i.modifiers.command));
            if ctrl && scroll.y != 0.0 {
                if let Some(p) = resp.hover_pos() {
                    self.zoom_at(rect, p, 1.0 + scroll.y * 0.002);
                }
            } else if scroll != Vec2::ZERO {
                self.pan += scroll;
            }
            let cursor = match (self.tool, &self.drag) {
                (_, Some(Drag::Pan)) => egui::CursorIcon::Grabbing,
                (Tool::Pan, _) => egui::CursorIcon::Grab,
                (Tool::Relation, _) => egui::CursorIcon::Crosshair,
                _ => egui::CursorIcon::Default,
            };
            ui.ctx().set_cursor_icon(cursor);
        }
        let selection: Vec<String> = self.selected.iter().cloned().collect();
        resp.context_menu(|ui| {
            if let Some(n) = selection.first().filter(|_| selection.len() == 1) {
                if ui.button(tr("Open Table")).clicked() {
                    event = Some(ErEvent::Open(n.clone()));
                    ui.close_menu();
                }
                if ui.button(tr("Design Table")).clicked() {
                    event = Some(ErEvent::Design(n.clone()));
                    ui.close_menu();
                }
                ui.separator();
            }
            if ui.button(tr("New Table")).clicked() {
                event = Some(ErEvent::NewTable);
                ui.close_menu();
            }
            if ui.button(tr("Auto Layout")).clicked() {
                self.auto_layout();
                ui.close_menu();
            }
            if ui.button(tr("Fit to Screen")).clicked() {
                self.fit_pending = true;
                ui.close_menu();
            }
        });

        // Bottom bar: refresh, tools and color.
        let bar = Rect::from_min_max(Pos2::new(full.min.x, rect.max.y), full.max);
        ui.painter().rect_filled(bar, 0.0, theme::panel(ui.ctx()));
        ui.painter().hline(bar.x_range(), bar.top(), Stroke::new(1.0, theme::border(ui.ctx())));
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(bar.shrink2(Vec2::new(6.0, 3.0))), |ui| {
            ui.horizontal_centered(|ui| {
                if ui.button(format!("{}  {}", icon::ARROW_CLOCKWISE, tr("Refresh"))).clicked() {
                    event = Some(ErEvent::Refresh);
                }
                ui.menu_button(icon::CARET_DOWN, |ui| {
                    if ui.button(tr("Refresh")).clicked() {
                        event = Some(ErEvent::Refresh);
                        ui.close_menu();
                    }
                    if ui.button(tr("Auto Layout")).clicked() {
                        self.auto_layout();
                        ui.close_menu();
                    }
                    if ui.button(tr("Fit to Screen")).clicked() {
                        self.fit_pending = true;
                        ui.close_menu();
                    }
                });
                ui.separator();
                for (t, ic, tip) in [
                    (Tool::Select, icon::CURSOR, tr("Select")),
                    (Tool::Pan, icon::HAND, tr("Pan")),
                    (Tool::Relation, icon::FLOW_ARROW, tr("Relation: drag from one column to another to create a foreign key")),
                ] {
                    if ui.selectable_label(self.tool == t, egui::RichText::new(ic).size(15.0)).on_hover_text(tip).clicked() {
                        self.tool = t;
                    }
                }
                ui.separator();
                for (st, ic, tip) in [
                    (LineStyle::Curves, icon::BEZIER_CURVE, tr("Curved relations from column to column")),
                    (LineStyle::Orthogonal, icon::STEPS, tr("Right-angle relations (Navicat style)")),
                ] {
                    if ui.selectable_label(self.style == st, egui::RichText::new(ic).size(15.0)).on_hover_text(tip).clicked() {
                        self.style = st;
                        self.routes_key = 0;
                    }
                }
                ui.separator();
                let current = self.selected.iter().next().map(|n| self.header(n)).unwrap_or_else(default_header);
                ui.add_enabled_ui(!self.selected.is_empty(), |ui| {
                    ui.menu_button(egui::RichText::new(format!("{}  ▾", icon::PALETTE)).color(current), |ui| {
                        for (name, c) in PALETTE {
                            if ui.button(egui::RichText::new(format!("■  {}", tr(name))).color(c)).clicked() {
                                for n in &self.selected {
                                    if c == default_header() {
                                        self.colors.remove(n);
                                    } else {
                                        self.colors.insert(n.clone(), c);
                                    }
                                }
                                self.moved = true;
                                ui.close_menu();
                            }
                        }
                    })
                    .response
                    .on_hover_text(tr("Color of selected tables"));
                });
                if !self.selected.is_empty() {
                    ui.label(egui::RichText::new(trf!("{n} selected", n = self.selected.len())).size(11.0).color(theme::MUTED));
                }
            });
        });
        event
    }

    /// Minimap and zoom at the bottom right. Returns whether the pointer is over it.
    fn minimap(&mut self, ui: &egui::Ui, rect: Rect, origin: Pos2, painter: &egui::Painter) -> bool {
        if self.tables.is_empty() || rect.width() < 400.0 || rect.height() < 300.0 {
            return false;
        }
        let dark = ui.visuals().dark_mode;
        let z = self.zoom;
        let mini = Rect::from_min_size(rect.right_bottom() - Vec2::new(196.0, 162.0), Vec2::new(184.0, 120.0));
        let controls = Rect::from_min_size(Pos2::new(mini.left(), mini.bottom() + 6.0), Vec2::new(mini.width(), 24.0));
        let panel = mini.union(controls).expand(6.0);
        painter.rect(
            panel,
            4.0,
            if dark { Color32::from_rgb(0x2B, 0x2D, 0x30) } else { Color32::from_rgb(0xF4, 0xF5, 0xF7) },
            Stroke::new(1.0, theme::border(ui.ctx())),
        );
        painter.rect(mini, 0.0, if dark { Color32::from_rgb(0x1E, 0x1F, 0x22) } else { Color32::WHITE }, Stroke::new(1.0, theme::border(ui.ctx())));
        let view = Rect::from_min_max(((rect.min - origin) / z).to_pos2(), ((rect.max - origin) / z).to_pos2());
        let world = self.bounds().unwrap_or(view).union(view).expand(20.0);
        let scale = (mini.width() / world.width()).min(mini.height() / world.height());
        let to_mini = |p: Pos2| mini.min + (p - world.min) * scale;
        for t in &self.tables {
            if let Some(wr) = self.world_rect(t) {
                painter.rect_filled(Rect::from_min_max(to_mini(wr.min), to_mini(wr.max)), 1.0, self.header(&t.name));
            }
        }
        painter.rect_stroke(Rect::from_min_max(to_mini(view.min), to_mini(view.max)).intersect(mini), 0.0, Stroke::new(1.5, theme::ORANGE));
        let mr = ui.interact(mini, ui.id().with("er_minimap"), Sense::click_and_drag());
        if let Some(p) = mr.interact_pointer_pos().filter(|_| mr.dragged() || mr.clicked()) {
            let target = world.min + (p - mini.min) / scale;
            self.pan = rect.center() - rect.min - target.to_vec2() * z;
        }
        let b = 22.0;
        let minus = Rect::from_min_size(controls.min, Vec2::splat(b));
        let fit = Rect::from_min_size(Pos2::new(controls.right() - b, controls.top()), Vec2::splat(b));
        let plus = Rect::from_min_size(Pos2::new(fit.left() - b - 4.0, controls.top()), Vec2::splat(b));
        let track = Rect::from_min_max(Pos2::new(minus.right() + 8.0, controls.center().y - 2.0), Pos2::new(plus.left() - 8.0, controls.center().y + 2.0));
        for (r, ic, tip) in [(minus, icon::MINUS, "Zoom Out"), (plus, icon::PLUS, "Zoom In"), (fit, icon::CORNERS_OUT, "Fit to Screen")] {
            let br = ui.interact(r, ui.id().with(("er_zoom", tip)), Sense::click()).on_hover_text(tr(tip));
            if br.hovered() {
                painter.rect_filled(r, 3.0, theme::hover(ui.ctx()));
            }
            painter.text(r.center(), Align2::CENTER_CENTER, ic, FontId::proportional(14.0), theme::text(ui.ctx()));
            if br.clicked() {
                match tip {
                    "Zoom Out" => self.zoom_at(rect, rect.center(), 1.0 / 1.2),
                    "Zoom In" => self.zoom_at(rect, rect.center(), 1.2),
                    _ => self.fit_pending = true,
                }
            }
        }
        painter.rect_filled(track, 2.0, theme::border(ui.ctx()));
        let frac = (self.zoom.ln() - 0.3f32.ln()) / (3.0f32.ln() - 0.3f32.ln());
        let knob = Pos2::new(track.left() + frac * track.width(), track.center().y);
        painter.circle(knob, 6.0, theme::bg(ui.ctx()), Stroke::new(1.5, theme::BLUE));
        let sr = ui.interact(track.expand2(Vec2::new(6.0, 8.0)), ui.id().with("er_zoom_slider"), Sense::click_and_drag());
        if let Some(p) = sr.interact_pointer_pos().filter(|_| sr.dragged() || sr.clicked()) {
            let f = ((p.x - track.left()) / track.width()).clamp(0.0, 1.0);
            let zoom = (0.3f32.ln() + f * (3.0f32.ln() - 0.3f32.ln())).exp();
            self.zoom_at(rect, rect.center(), zoom / self.zoom);
        }
        ui.ctx().pointer_hover_pos().is_some_and(|p| panel.contains(p))
    }

    /// Changes the zoom keeping the screen point `at` fixed.
    fn zoom_at(&mut self, rect: Rect, at: Pos2, factor: f32) {
        let old = self.zoom;
        self.zoom = (self.zoom * factor).clamp(0.3, 3.0);
        let world = (at - rect.min - self.pan) / old;
        self.pan = at - rect.min - world * self.zoom;
    }
}

/// Linear blend of two colors (t between 0 and 1).
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

fn snap(v: f32) -> f32 {
    (v / GRID).round() * GRID
}

/// Removes intermediate points on straight segments.
fn simplify(pts: Vec<Pos2>) -> Vec<Pos2> {
    let mut out: Vec<Pos2> = Vec::with_capacity(pts.len());
    for p in pts {
        if out.len() >= 2 {
            let a = out[out.len() - 2];
            let b = out[out.len() - 1];
            let collinear = ((a.x - b.x).abs() < 0.5 && (b.x - p.x).abs() < 0.5) || ((a.y - b.y).abs() < 0.5 && (b.y - p.y).abs() < 0.5);
            if collinear {
                out.pop();
            }
        }
        if out.last() != Some(&p) {
            out.push(p);
        }
    }
    out
}

/// Ends and exit direction of a wire between row `ya` of `a` and row `yb` of `b`.
/// Returns (start, exit direction, end, arrival direction) with direction ±1 in x.
fn wire(a: Rect, ya: f32, b: Rect, yb: f32, self_ref: bool) -> (Pos2, f32, Pos2, f32) {
    if self_ref {
        // Loop around the right side of the table itself.
        return (Pos2::new(a.right(), ya), 1.0, Pos2::new(a.right(), yb), 1.0);
    }
    if b.left() >= a.right() {
        (Pos2::new(a.right(), ya), 1.0, Pos2::new(b.left(), yb), -1.0)
    } else if b.right() <= a.left() {
        (Pos2::new(a.left(), ya), -1.0, Pos2::new(b.right(), yb), 1.0)
    } else if a.center().x <= b.center().x {
        // Horizontally overlapping: leave and arrive on the same side, making a loop.
        (Pos2::new(a.right(), ya), 1.0, Pos2::new(b.right(), yb), 1.0)
    } else {
        (Pos2::new(a.left(), ya), -1.0, Pos2::new(b.left(), yb), -1.0)
    }
}

/// Provisional route (while a table is being dragged).
fn elbow(a: Rect, b: Rect) -> Vec<Pos2> {
    let (x1, x2) = if a.center().x < b.center().x { (a.right(), b.left()) } else { (a.left(), b.right()) };
    let mid = (x1 + x2) / 2.0;
    vec![Pos2::new(x1, a.center().y), Pos2::new(mid, a.center().y), Pos2::new(mid, b.center().y), Pos2::new(x2, b.center().y)]
}

#[cfg(test)]
mod tests {

    #[test]
    fn cycles_are_broken_for_layout() {
        // departments → employees → positions → departments, plus orders → departments.
        let es = [(0, 1), (1, 2), (2, 0), (3, 0)];
        let dag = super::acyclic(&es, &[0, 1, 2, 3]);
        assert_eq!(dag.len(), 3, "exactly one reference of the cycle is dropped: {dag:?}");
        assert!(dag.contains(&(3, 0)));
    }

    use super::*;
    use lumedb_core::ddl::{ColumnDef, ForeignKeyDef};

    fn table(name: &str, refs: &[&str]) -> TableDesign {
        TableDesign {
            name: name.into(),
            columns: (0..4).map(|i| ColumnDef { name: format!("c{i}"), data_type: "int".into(), ..Default::default() }).collect(),
            foreign_keys: refs
                .iter()
                .map(|r| ForeignKeyDef {
                    name: format!("fk_{name}_{r}"),
                    columns: vec!["c1".into()],
                    ref_table: r.to_string(),
                    ref_columns: vec!["c0".into()],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn layout_and_routes_avoid_boxes_and_each_other() {
        let mut c = ErCanvas::default();
        c.set_tables(vec![
            table("clientes", &[]),
            table("productos", &[]),
            table("pedidos", &["clientes"]),
            table("lineas", &["pedidos", "productos"]),
            table("facturas", &["pedidos", "clientes"]),
            table("pagos", &["facturas"]),
            table("suelta", &[]),
        ]);
        c.do_layout();
        c.compute_routes();
        let rects: Vec<Rect> = c.tables.iter().map(|t| c.world_rect(t).unwrap()).collect();
        // No box overlaps another.
        for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                assert!(!rects[i].intersects(rects[j]), "{} and {} overlap", c.tables[i].name, c.tables[j].name);
            }
        }
        // Every relation has a route and none crosses a box.
        let es = edges(&c.tables);
        assert_eq!(c.routes.len(), es.len());
        let on_edge = |r: Rect, p: Pos2| {
            let near = |a: f32, b: f32| (a - b).abs() < 0.6;
            ((near(p.x, r.left()) || near(p.x, r.right())) && r.y_range().contains(p.y))
                || ((near(p.y, r.top()) || near(p.y, r.bottom())) && r.x_range().contains(p.x))
        };
        for (k, &(a, b)) in es.iter().enumerate() {
            let r = &c.routes[k];
            assert!(on_edge(rects[a], r[0]), "relation {k} does not leave from the edge of {}", c.tables[a].name);
            assert!(on_edge(rects[b], *r.last().unwrap()), "relation {k} does not reach the edge of {}", c.tables[b].name);
        }
        let mut segments: Vec<(Pos2, Pos2)> = Vec::new();
        for (k, r) in c.routes.iter().enumerate() {
            assert!(r.len() >= 2, "relation {k} has no route");
            for w in r.windows(2) {
                let steps = ((w[1] - w[0]).length() / 2.0).ceil() as usize;
                for s in 1..steps {
                    let p = w[0] + (w[1] - w[0]) * (s as f32 / steps as f32);
                    for (ti, b) in rects.iter().enumerate() {
                        assert!(!b.shrink(1.0).contains(p), "relation {k} crosses {}", c.tables[ti].name);
                    }
                }
                segments.push((w[0], w[1]));
            }
        }
        // No pair of relations shares a segment (collinear overlaps).
        let mut overlap = 0;
        for (i, a) in segments.iter().enumerate() {
            for b in &segments[i + 1..] {
                let horizontal = |s: &(Pos2, Pos2)| (s.0.y - s.1.y).abs() < 0.5;
                if horizontal(a) && horizontal(b) && (a.0.y - b.0.y).abs() < 0.5 {
                    let (a0, a1) = (a.0.x.min(a.1.x), a.0.x.max(a.1.x));
                    let (b0, b1) = (b.0.x.min(b.1.x), b.0.x.max(b.1.x));
                    if a1.min(b1) - a0.max(b0) > GRID {
                        overlap += 1;
                    }
                }
                if !horizontal(a) && !horizontal(b) && (a.0.x - b.0.x).abs() < 0.5 {
                    let (a0, a1) = (a.0.y.min(a.1.y), a.0.y.max(a.1.y));
                    let (b0, b1) = (b.0.y.min(b.1.y), b.0.y.max(b.1.y));
                    if a1.min(b1) - a0.max(b0) > GRID {
                        overlap += 1;
                    }
                }
            }
        }
        assert_eq!(overlap, 0, "some relations share a segment");
        // Referenced tables end up left of the tables that reference them.
        let x = |n: &str| c.pos[n].x;
        assert!(x("clientes") < x("pedidos") && x("pedidos") < x("lineas") && x("facturas") < x("pagos"));
    }

    #[test]
    fn moving_a_table_only_reroutes_its_relations() {
        let mut c = ErCanvas::default();
        c.set_tables(vec![
            table("clientes", &[]),
            table("productos", &[]),
            table("pedidos", &["clientes"]),
            table("lineas", &["pedidos", "productos"]),
            table("facturas", &["pedidos", "clientes"]),
            table("pagos", &["facturas"]),
        ]);
        c.do_layout();
        c.compute_routes();
        let before = c.routes.clone();
        // Move "productos" well down, as when dragging it.
        c.selected.insert("productos".into());
        *c.pos.get_mut("productos").unwrap() += Vec2::new(0.0, 400.0);
        let affected = c.affected_edges();
        c.route_edges(Some(&affected));
        let es = edges(&c.tables);
        for (k, &(a, b)) in es.iter().enumerate() {
            let touches = c.tables[a].name == "productos" || c.tables[b].name == "productos";
            if !affected.contains(&k) {
                assert_eq!(c.routes[k], before[k], "relation {k} changed for no reason");
            }
            assert!(c.routes[k].len() >= 2, "relation {k} has no route");
            if touches {
                assert!(affected.contains(&k));
            }
        }
        let rects: Vec<Rect> = c.tables.iter().map(|t| c.world_rect(t).unwrap()).collect();
        for r in &c.routes {
            for w in r.windows(2) {
                let steps = ((w[1] - w[0]).length() / 2.0).ceil() as usize;
                for s in 1..steps {
                    let p = w[0] + (w[1] - w[0]) * (s as f32 / steps as f32);
                    assert!(!rects.iter().any(|b| b.shrink(1.0).contains(p)), "a relation crosses a box after moving");
                }
            }
        }
    }

    #[test]
    fn wires_leave_from_the_facing_side() {
        let a = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(100.0, 200.0));
        let right = Rect::from_min_size(Pos2::new(300.0, 50.0), Vec2::new(100.0, 100.0));
        let left = Rect::from_min_size(Pos2::new(-300.0, 50.0), Vec2::new(100.0, 100.0));
        let below = Rect::from_min_size(Pos2::new(20.0, 400.0), Vec2::new(100.0, 100.0));
        assert_eq!(wire(a, 30.0, right, 70.0, false), (Pos2::new(100.0, 30.0), 1.0, Pos2::new(300.0, 70.0), -1.0));
        assert_eq!(wire(a, 30.0, left, 70.0, false), (Pos2::new(0.0, 30.0), -1.0, Pos2::new(-200.0, 70.0), 1.0));
        let (p0, d0, p3, d3) = wire(a, 30.0, below, 420.0, false);
        assert_eq!((p0.x, d0, p3.x, d3), (100.0, 1.0, 120.0, 1.0));
        let (p0, _, p3, _) = wire(a, 30.0, a, 10.0, true);
        assert_eq!((p0.x, p3.x), (100.0, 100.0));
    }
}
