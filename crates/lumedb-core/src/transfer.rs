//! Data export and import wizards.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};

use crate::config::DbKind;
use crate::driver::{QueryResult, Value};
use crate::{tr, trf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Txt,
    Json,
    Xml,
    Html,
    Markdown,
    Sql,
    Xlsx,
}

impl ExportFormat {
    pub const ALL: [ExportFormat; 8] = [
        ExportFormat::Csv,
        ExportFormat::Txt,
        ExportFormat::Json,
        ExportFormat::Xml,
        ExportFormat::Html,
        ExportFormat::Markdown,
        ExportFormat::Sql,
        ExportFormat::Xlsx,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Csv => tr("CSV file (*.csv)"),
            ExportFormat::Txt => tr("Text file (*.txt)"),
            ExportFormat::Json => tr("JSON file (*.json)"),
            ExportFormat::Xml => tr("XML file (*.xml)"),
            ExportFormat::Html => tr("HTML file (*.html)"),
            ExportFormat::Markdown => tr("Markdown (*.md)"),
            ExportFormat::Sql => tr("SQL script (*.sql)"),
            ExportFormat::Xlsx => tr("Excel (*.xlsx)"),
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Csv => "csv",
            ExportFormat::Txt => "txt",
            ExportFormat::Json => "json",
            ExportFormat::Xml => "xml",
            ExportFormat::Html => "html",
            ExportFormat::Markdown => "md",
            ExportFormat::Sql => "sql",
            ExportFormat::Xlsx => "xlsx",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub format: ExportFormat,
    pub include_header: bool,
    /// Field delimiter (CSV).
    pub delimiter: u8,
    /// Text that represents NULL in text formats.
    pub null_text: String,
    /// Target table of the INSERTs (SQL) and name of the sheet/element.
    pub table_name: String,
    pub dialect: DbKind,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self { format: ExportFormat::Csv, include_header: true, delimiter: b',', null_text: String::new(), table_name: "table".into(), dialect: DbKind::Mysql }
    }
}

fn text(v: &Value, null: &str) -> String {
    v.to_text().unwrap_or_else(|| null.to_string())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn json_value(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Number(n) => n.parse::<serde_json::Number>().map(serde_json::Value::Number).unwrap_or_else(|_| n.clone().into()),
        Value::Text(s) => s.clone().into(),
        Value::Bytes(b) => hex::encode(b).into(),
    }
}

/// Writes the result to `path` in the chosen format.
pub fn export(r: &QueryResult, path: &Path, o: &ExportOptions) -> Result<()> {
    if o.format == ExportFormat::Xlsx {
        return export_xlsx(r, path, o);
    }
    let mut w = BufWriter::new(File::create(path).with_context(|| trf!("could not create {path}", path = path.display()))?);
    write_export(r, &mut w, o)?;
    w.flush()?;
    Ok(())
}

pub fn write_export(r: &QueryResult, w: &mut dyn Write, o: &ExportOptions) -> Result<()> {
    match o.format {
        ExportFormat::Csv | ExportFormat::Txt => {
            let delim = if o.format == ExportFormat::Txt { b'\t' } else { o.delimiter };
            let mut c = csv::WriterBuilder::new().delimiter(delim).from_writer(w);
            if o.include_header {
                c.write_record(&r.columns)?;
            }
            for row in &r.rows {
                c.write_record(row.iter().map(|v| text(v, &o.null_text)))?;
            }
            c.flush()?;
        }
        ExportFormat::Json => {
            let rows: Vec<serde_json::Value> = r
                .rows
                .iter()
                .map(|row| {
                    let obj: serde_json::Map<String, serde_json::Value> = r.columns.iter().cloned().zip(row.iter().map(json_value)).collect();
                    serde_json::Value::Object(obj)
                })
                .collect();
            serde_json::to_writer_pretty(&mut *w, &rows)?;
            writeln!(w)?;
        }
        ExportFormat::Xml => {
            writeln!(w, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
            writeln!(w, "<{}>", xml_tag(&o.table_name))?;
            for row in &r.rows {
                writeln!(w, "  <row>")?;
                for (c, v) in r.columns.iter().zip(row) {
                    let tag = xml_tag(c);
                    match v.to_text() {
                        Some(t) => writeln!(w, "    <{tag}>{}</{tag}>", xml_escape(&t))?,
                        None => writeln!(w, "    <{tag} null=\"true\"/>")?,
                    }
                }
                writeln!(w, "  </row>")?;
            }
            writeln!(w, "</{}>", xml_tag(&o.table_name))?;
        }
        ExportFormat::Html => {
            writeln!(w, "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>{}</title>", xml_escape(&o.table_name))?;
            writeln!(
                w,
                "<style>table{{border-collapse:collapse;font:13px sans-serif}}td,th{{border:1px solid #ccc;padding:3px 6px}}th{{background:#eee}}</style></head><body><table>"
            )?;
            if o.include_header {
                write!(w, "<tr>")?;
                for c in &r.columns {
                    write!(w, "<th>{}</th>", xml_escape(c))?;
                }
                writeln!(w, "</tr>")?;
            }
            for row in &r.rows {
                write!(w, "<tr>")?;
                for v in row {
                    write!(w, "<td>{}</td>", xml_escape(&text(v, &o.null_text)))?;
                }
                writeln!(w, "</tr>")?;
            }
            writeln!(w, "</table></body></html>")?;
        }
        ExportFormat::Markdown => {
            let esc = |s: String| s.replace('|', "\\|").replace('\n', " ");
            writeln!(w, "| {} |", r.columns.iter().map(|c| esc(c.clone())).collect::<Vec<_>>().join(" | "))?;
            writeln!(w, "|{}|", vec!["---"; r.columns.len()].join("|"))?;
            for row in &r.rows {
                writeln!(w, "| {} |", row.iter().map(|v| esc(text(v, &o.null_text))).collect::<Vec<_>>().join(" | "))?;
            }
        }
        ExportFormat::Sql => {
            for stmt in insert_statements(o.dialect, "", &o.table_name, &r.columns, &r.rows, 100) {
                writeln!(w, "{stmt};")?;
            }
        }
        ExportFormat::Xlsx => unreachable!(),
    }
    Ok(())
}

fn xml_tag(s: &str) -> String {
    let t: String = s.chars().map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    if t.starts_with(|c: char| c.is_alphabetic() || c == '_') {
        t
    } else {
        format!("_{t}")
    }
}

fn export_xlsx(r: &QueryResult, path: &Path, o: &ExportOptions) -> Result<()> {
    use rust_xlsxwriter::{Format, Workbook};
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    let name: String = o.table_name.chars().filter(|c| !"[]:*?/\\".contains(*c)).take(31).collect();
    if !name.is_empty() {
        ws.set_name(&name)?;
    }
    let bold = Format::new().set_bold();
    let mut row0 = 0u32;
    if o.include_header {
        for (c, name) in r.columns.iter().enumerate() {
            ws.write_string_with_format(0, c as u16, name, &bold)?;
        }
        row0 = 1;
    }
    for (i, row) in r.rows.iter().enumerate() {
        for (c, v) in row.iter().enumerate() {
            let (rr, cc) = (row0 + i as u32, c as u16);
            match v {
                Value::Null => {}
                Value::Number(n) => match n.parse::<f64>() {
                    Ok(f) if f.is_finite() => {
                        ws.write_number(rr, cc, f)?;
                    }
                    _ => {
                        ws.write_string(rr, cc, n)?;
                    }
                },
                v => {
                    ws.write_string(rr, cc, v.to_text().unwrap_or_default())?;
                }
            }
        }
    }
    wb.save(path)?;
    Ok(())
}

/// Multi-row `INSERT`s in batches of `batch` rows.
pub fn insert_statements(kind: DbKind, schema: &str, table: &str, columns: &[String], rows: &[Vec<Value>], batch: usize) -> Vec<String> {
    let t = kind.qualified(schema, table);
    let cols = columns.iter().map(|c| kind.quote_one(c)).collect::<Vec<_>>().join(", ");
    rows.chunks(batch.max(1))
        .map(|chunk| {
            let values: Vec<String> = chunk.iter().map(|r| format!("({})", r.iter().map(|v| v.sql_literal(kind)).collect::<Vec<_>>().join(", "))).collect();
            format!("INSERT INTO {t} ({cols}) VALUES\n{}", values.join(",\n"))
        })
        .collect()
}

// ---------------------------------------------------------------- Import

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportFormat {
    Csv,
    Txt,
    Json,
    Xlsx,
}

impl ImportFormat {
    pub const ALL: [ImportFormat; 4] = [ImportFormat::Csv, ImportFormat::Txt, ImportFormat::Json, ImportFormat::Xlsx];

    pub fn label(self) -> &'static str {
        match self {
            ImportFormat::Csv => tr("CSV file (*.csv)"),
            ImportFormat::Txt => tr("Text file (*.txt)"),
            ImportFormat::Json => tr("JSON file (*.json)"),
            ImportFormat::Xlsx => tr("Excel (*.xlsx, *.xls, *.ods)"),
        }
    }

    pub fn from_path(p: &Path) -> Self {
        match p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
            "json" => ImportFormat::Json,
            "xlsx" | "xls" | "ods" => ImportFormat::Xlsx,
            "txt" | "tsv" => ImportFormat::Txt,
            _ => ImportFormat::Csv,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub format: ImportFormat,
    pub has_header: bool,
    pub delimiter: u8,
    /// Text read as NULL (empty = none).
    pub null_text: String,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self { format: ImportFormat::Csv, has_header: true, delimiter: b',', null_text: String::new() }
    }
}

/// Reads a file into an in-memory table (every cell as text or number).
pub fn read_file(path: &Path, o: &ImportOptions) -> Result<QueryResult> {
    let cell = |s: String| if !o.null_text.is_empty() && s == o.null_text { Value::Null } else { Value::Text(s) };
    let mut r = QueryResult::default();
    match o.format {
        ImportFormat::Csv | ImportFormat::Txt => {
            let delim = if o.format == ImportFormat::Txt { b'\t' } else { o.delimiter };
            let mut rd = csv::ReaderBuilder::new().delimiter(delim).has_headers(false).flexible(true).from_path(path)?;
            for (i, rec) in rd.records().enumerate() {
                let rec = rec?;
                if i == 0 && o.has_header {
                    r.columns = rec.iter().map(|s| s.trim_start_matches('\u{feff}').to_string()).collect();
                    continue;
                }
                r.rows.push(rec.iter().map(|s| cell(s.to_string())).collect());
            }
        }
        ImportFormat::Json => {
            let v: serde_json::Value = serde_json::from_reader(std::io::BufReader::new(File::open(path)?))?;
            let arr = match v {
                serde_json::Value::Array(a) => a,
                serde_json::Value::Object(mut m) => match m.values_mut().find(|v| v.is_array()) {
                    Some(serde_json::Value::Array(a)) => std::mem::take(a),
                    _ => bail!(tr("the JSON must be a list of objects")),
                },
                _ => bail!(tr("the JSON must be a list of objects")),
            };
            for item in &arr {
                if let serde_json::Value::Object(m) = item {
                    for k in m.keys() {
                        if !r.columns.contains(k) {
                            r.columns.push(k.clone());
                        }
                    }
                }
            }
            for item in arr {
                let serde_json::Value::Object(m) = item else { continue };
                r.rows.push(
                    r.columns
                        .iter()
                        .map(|c| match m.get(c) {
                            None | Some(serde_json::Value::Null) => Value::Null,
                            Some(serde_json::Value::Number(n)) => Value::Number(n.to_string()),
                            Some(serde_json::Value::Bool(b)) => Value::Number(if *b { "1" } else { "0" }.into()),
                            Some(serde_json::Value::String(s)) => cell(s.clone()),
                            Some(other) => Value::Text(other.to_string()),
                        })
                        .collect(),
                );
            }
        }
        ImportFormat::Xlsx => {
            use calamine::{open_workbook_auto, Data, Reader};
            let mut wb = open_workbook_auto(path)?;
            let sheet = wb.sheet_names().first().cloned().ok_or_else(|| anyhow!(tr("the workbook has no sheets")))?;
            let range = wb.worksheet_range(&sheet)?;
            for (i, row) in range.rows().enumerate() {
                let vals: Vec<Value> = row
                    .iter()
                    .map(|c| match c {
                        Data::Empty => Value::Null,
                        Data::Int(n) => Value::Number(n.to_string()),
                        Data::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => Value::Number(format!("{}", *f as i64)),
                        Data::Float(f) => Value::Number(f.to_string()),
                        Data::Bool(b) => Value::Number(if *b { "1" } else { "0" }.into()),
                        other => cell(other.to_string()),
                    })
                    .collect();
                if i == 0 && o.has_header {
                    r.columns = vals.iter().map(|v| v.to_text().unwrap_or_default()).collect();
                } else {
                    r.rows.push(vals);
                }
            }
        }
    }
    let width = r.rows.iter().map(|row| row.len()).max().unwrap_or(0).max(r.columns.len());
    for i in r.columns.len()..width {
        r.columns.push(format!("column{}", i + 1));
    }
    for row in &mut r.rows {
        row.resize(width, Value::Null);
    }
    Ok(r)
}

/// New table for an import: every column as text.
pub fn create_table_for_import(kind: DbKind, schema: &str, table: &str, columns: &[String]) -> String {
    let ty = match kind {
        DbKind::Mysql => "TEXT",
        DbKind::Postgres => "text",
        DbKind::Sqlite => "TEXT",
    };
    let cols: Vec<String> = columns.iter().map(|c| format!("  {} {ty}", kind.quote_one(c))).collect();
    format!("CREATE TABLE {} (\n{}\n)", kind.qualified(schema, table), cols.join(",\n"))
}
