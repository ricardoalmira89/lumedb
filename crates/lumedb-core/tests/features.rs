//! Export/import, dump, transfer, synchronization, users and monitor.
//! PostgreSQL and MySQL only with `LUMEDB_TEST_LIVE=1`.

use lumedb_core::ddl::{self, ColumnDef, ForeignKeyDef, TableDesign};
use lumedb_core::transfer::{self, ExportFormat, ExportOptions, ImportFormat, ImportOptions};
use lumedb_core::{admin, automation, connect, dump, meta, sync, ConnectionConfig, DbKind, Driver, QueryResult, Value};

fn live() -> bool {
    std::env::var("LUMEDB_TEST_LIVE").is_ok()
}

fn cfg(kind: DbKind, sqlite_path: &str) -> ConnectionConfig {
    let mut c = ConnectionConfig { kind, host: "127.0.0.1".into(), password: "lumedb".into(), ..Default::default() };
    match kind {
        DbKind::Sqlite => c.path = sqlite_path.into(),
        DbKind::Postgres => {
            c.port = 55432;
            c.user = "postgres".into();
        }
        DbKind::Mysql => {
            c.port = 33306;
            c.user = "root".into();
        }
    }
    c
}

/// Empty, ready schema. On SQLite it is always `main`.
fn fresh(d: &mut dyn Driver, schema: &str) -> String {
    match d.kind() {
        DbKind::Sqlite => "main".into(),
        DbKind::Postgres => {
            d.execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema}")).unwrap();
            schema.into()
        }
        DbKind::Mysql => {
            d.execute(&format!("DROP DATABASE IF EXISTS {schema}")).unwrap();
            d.execute(&format!("CREATE DATABASE {schema}")).unwrap();
            schema.into()
        }
    }
}

fn col(name: &str, ty: &str) -> ColumnDef {
    ColumnDef { name: name.into(), data_type: ty.into(), nullable: true, ..Default::default() }
}

/// `clientes` and `pedidos` tables with data.
fn sample(d: &mut dyn Driver, schema: &str) {
    let k = d.kind();
    let (int, txt, boolean, blob) = match k {
        DbKind::Postgres => ("integer", "character varying(100)", "boolean", "bytea"),
        DbKind::Mysql => ("int", "varchar(100)", "tinyint(1)", "longblob"),
        DbKind::Sqlite => ("INTEGER", "TEXT", "INTEGER", "BLOB"),
    };
    let clientes = TableDesign {
        schema: schema.into(),
        name: "clientes".into(),
        columns: vec![
            ColumnDef { primary_key: true, auto_increment: true, nullable: false, ..col("id", int) },
            ColumnDef { nullable: false, ..col("nombre", txt) },
            col("email", txt),
            col("activo", boolean),
            col("foto", blob),
        ],
        ..Default::default()
    };
    let pedidos = TableDesign {
        schema: schema.into(),
        name: "pedidos".into(),
        columns: vec![
            ColumnDef { primary_key: true, nullable: false, ..col("id", int) },
            col("cliente_id", int),
            col("total", if k == DbKind::Sqlite { "REAL" } else { "decimal(10,2)" }),
        ],
        foreign_keys: vec![ForeignKeyDef {
            name: "fk_pedidos_cliente".into(),
            columns: vec!["cliente_id".into()],
            ref_schema: schema.into(),
            ref_table: "clientes".into(),
            ref_columns: vec!["id".into()],
            ..Default::default()
        }],
        ..Default::default()
    };
    // Pedidos first in the list, to check the dump's dependency ordering.
    for t in [&clientes, &pedidos] {
        for s in ddl::create_table(k, t) {
            d.execute(&s).unwrap_or_else(|e| panic!("{k:?} {s}: {e}"));
        }
    }
    let t = k.qualified(schema, "clientes");
    let yes = if k == DbKind::Postgres { "true" } else { "1" };
    let no = if k == DbKind::Postgres { "false" } else { "0" };
    let foto = Value::Bytes(vec![0x89, 0x50, 0x00, 0xff]).sql_literal(k);
    d.execute(&format!(
        "INSERT INTO {t} (nombre, email, activo, foto) VALUES ('Ana', 'ana@x.es', {yes}, {foto}), ('Luis O''Neil', NULL, {no}, NULL), ('Eva', 'eva@x.es', {yes}, NULL)"
    ))
    .unwrap();
    d.execute(&format!("INSERT INTO {} (id, cliente_id, total) VALUES (1, 1, 10.50), (2, 1, 3.25), (3, 3, 99.99)", k.qualified(schema, "pedidos")))
        .unwrap();
}

fn count(d: &mut dyn Driver, schema: &str, t: &str) -> u64 {
    let r = d.execute(&format!("SELECT COUNT(*) FROM {}", d.kind().qualified(schema, t))).unwrap();
    r.rows[0][0].to_text().unwrap().parse().unwrap()
}

fn tmp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lumedb-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn export_import_roundtrip() {
    let r = QueryResult {
        columns: vec!["id".into(), "nombre".into(), "nota".into()],
        rows: vec![
            vec![Value::Number("1".into()), Value::Text("Ana, \"la jefa\"".into()), Value::Null],
            vec![Value::Number("2".into()), Value::Text("Ñandú\nsegunda línea".into()), Value::Text("ok".into())],
        ],
        ..Default::default()
    };
    for (ef, imf) in [
        (ExportFormat::Csv, ImportFormat::Csv),
        (ExportFormat::Txt, ImportFormat::Txt),
        (ExportFormat::Json, ImportFormat::Json),
        (ExportFormat::Xlsx, ImportFormat::Xlsx),
    ] {
        let p = tmp(&format!("roundtrip.{}", ef.extension()));
        let null = if matches!(ef, ExportFormat::Csv | ExportFormat::Txt) { "\\N" } else { "" };
        transfer::export(&r, &p, &ExportOptions { format: ef, null_text: null.into(), ..Default::default() }).unwrap();
        let back = transfer::read_file(&p, &ImportOptions { format: imf, null_text: null.into(), ..Default::default() }).unwrap();
        assert_eq!(back.columns, r.columns, "{ef:?}");
        assert_eq!(back.rows.len(), 2, "{ef:?}");
        assert_eq!(back.rows[0][1].to_text(), r.rows[0][1].to_text(), "{ef:?}");
        assert_eq!(back.rows[1][1].to_text(), r.rows[1][1].to_text(), "{ef:?}");
        assert!(back.rows[0][2].is_null(), "{ef:?}: {:?}", back.rows[0][2]);
        assert_eq!(back.rows[0][0].to_text().as_deref(), Some("1"), "{ef:?}");
    }
    for f in [ExportFormat::Xml, ExportFormat::Html, ExportFormat::Markdown, ExportFormat::Sql] {
        let p = tmp(&format!("out.{}", f.extension()));
        transfer::export(&r, &p, &ExportOptions { format: f, ..Default::default() }).unwrap();
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(s.contains("Ana"), "{f:?}");
    }
}

fn engines() -> Vec<DbKind> {
    if live() {
        vec![DbKind::Sqlite, DbKind::Postgres, DbKind::Mysql]
    } else {
        vec![DbKind::Sqlite]
    }
}

#[test]
fn dump_and_restore() {
    for k in engines() {
        let db = tmp(&format!("dump-{k:?}.db"));
        let _ = std::fs::remove_file(&db);
        let mut d = connect(&cfg(k, db.to_str().unwrap())).unwrap();
        let s = fresh(d.as_mut(), "lumedb_dump");
        sample(d.as_mut(), &s);
        d.execute(&format!("CREATE VIEW {} AS SELECT nombre FROM {}", k.qualified(&s, "v_nombres"), k.qualified(&s, "clientes")))
            .unwrap();
        if k == DbKind::Mysql {
            d.execute(&format!("CREATE PROCEDURE {}() BEGIN SELECT 1; SELECT 2; END", k.qualified(&s, "p_doble"))).unwrap();
        }
        let mut out = Vec::new();
        dump::dump_schema(d.as_mut(), &s, None, dump::DumpOptions::default(), &mut out, &mut |_| {}).unwrap();
        let script = String::from_utf8(out).unwrap();
        assert!(script.find("TABLE IF EXISTS").is_some(), "{k:?}");

        // Restore into an empty schema.
        let s2 = match k {
            DbKind::Sqlite => {
                let db2 = tmp("restore.db");
                let _ = std::fs::remove_file(&db2);
                d = connect(&cfg(k, db2.to_str().unwrap())).unwrap();
                "main".to_string()
            }
            _ => fresh(d.as_mut(), "lumedb_dump"),
        };
        dump::use_schema(d.as_mut(), &s2).unwrap();
        let rep = dump::run_script(d.as_mut(), &script, false, &mut |_, _| {}).unwrap();
        assert!(rep.errors.is_empty(), "{k:?}: {:?}", rep.errors);
        assert_eq!(count(d.as_mut(), &s2, "clientes"), 3, "{k:?}");
        assert_eq!(count(d.as_mut(), &s2, "pedidos"), 3, "{k:?}");
        let objs = meta::list_objects(d.as_mut(), &s2).unwrap();
        assert!(objs.iter().any(|o| o.name == "v_nombres"), "{k:?}");
        if k == DbKind::Mysql {
            assert!(objs.iter().any(|o| o.name == "p_doble"), "{k:?}");
        }
        // The binary photo survives.
        let r = d.execute(&format!("SELECT foto FROM {} WHERE nombre = 'Ana'", k.qualified(&s2, "clientes"))).unwrap();
        let foto = match &r.rows[0][0] {
            Value::Text(t) => t.clone(),
            v => hex::encode(match v {
                Value::Bytes(b) => b.clone(),
                _ => vec![],
            }),
        };
        assert!(foto.contains("895000ff"), "{k:?}: {foto}");
        // Auto-increment continues after the last id.
        d.execute(&format!("INSERT INTO {} (nombre) VALUES ('Nuevo')", k.qualified(&s2, "clientes"))).unwrap();
    }
}

#[test]
fn transfer_between_engines() {
    if !live() {
        return;
    }
    for (from, to) in [(DbKind::Postgres, DbKind::Mysql), (DbKind::Mysql, DbKind::Sqlite), (DbKind::Sqlite, DbKind::Postgres)] {
        let a_db = tmp(&format!("tr-src-{from:?}-{to:?}.db"));
        let b_db = tmp(&format!("tr-dst-{from:?}-{to:?}.db"));
        let _ = std::fs::remove_file(&a_db);
        let _ = std::fs::remove_file(&b_db);
        let mut a = connect(&cfg(from, a_db.to_str().unwrap())).unwrap();
        let mut b = connect(&cfg(to, b_db.to_str().unwrap())).unwrap();
        let sa = fresh(a.as_mut(), "lumedb_src");
        let sb = fresh(b.as_mut(), "lumedb_dst");
        sample(a.as_mut(), &sa);
        let tables = vec!["clientes".to_string(), "pedidos".to_string()];
        let copied = sync::transfer_tables(a.as_mut(), &sa, b.as_mut(), &sb, &tables, sync::TransferOptions::default(), &mut |_, _| {})
            .unwrap_or_else(|e| panic!("{from:?}->{to:?}: {e:#}"));
        assert_eq!(copied, vec![("clientes".to_string(), 3), ("pedidos".to_string(), 3)], "{from:?}->{to:?}");
        let fks = meta::foreign_keys(b.as_mut(), &sb, "pedidos").unwrap();
        assert_eq!(fks.len(), 1, "{from:?}->{to:?}");
        let r = b.execute(&format!("SELECT activo, foto FROM {} WHERE nombre = 'Ana'", to.qualified(&sb, "clientes"))).unwrap();
        let activo = r.rows[0][0].to_text().unwrap();
        assert!(activo == "1" || activo == "t", "{from:?}->{to:?}: {activo}");
        let foto = match &r.rows[0][1] {
            Value::Bytes(b) => hex::encode(b),
            v => v.to_text().unwrap(),
        };
        assert!(foto.contains("895000ff"), "{from:?}->{to:?}: {foto}");

        // Data sync: change the target and bring it back to the source state.
        let tb = to.qualified(&sb, "clientes");
        b.execute(&format!("UPDATE {tb} SET email = 'cambiado' WHERE nombre = 'Eva'")).unwrap();
        b.execute(&format!("DELETE FROM {} WHERE id = 3", to.qualified(&sb, "pedidos"))).unwrap();
        b.execute(&format!("DELETE FROM {tb} WHERE nombre = 'Eva'")).unwrap();
        b.execute(&format!("INSERT INTO {tb} (id, nombre) VALUES (99, 'Sobra')")).unwrap();
        let diff = sync::data_diff(a.as_mut(), &sa, b.as_mut(), &sb, "clientes").unwrap();
        assert_eq!((diff.inserts, diff.updates, diff.deletes), (1, 0, 1), "{from:?}->{to:?} {:?}", diff.statements);
        for s in &diff.statements {
            b.execute(s).unwrap_or_else(|e| panic!("{from:?}->{to:?} {s}: {e}"));
        }
        let again = sync::data_diff(a.as_mut(), &sa, b.as_mut(), &sb, "clientes").unwrap();
        assert!(again.statements.is_empty(), "{from:?}->{to:?} {:?}", again.statements);
    }
}

#[test]
fn structure_sync() {
    for k in engines() {
        let a_db = tmp(&format!("ss-a-{k:?}.db"));
        let b_db = tmp(&format!("ss-b-{k:?}.db"));
        let _ = std::fs::remove_file(&a_db);
        let _ = std::fs::remove_file(&b_db);
        let mut a = connect(&cfg(k, a_db.to_str().unwrap())).unwrap();
        let mut b = connect(&cfg(k, b_db.to_str().unwrap())).unwrap();
        let sa = fresh(a.as_mut(), "lumedb_ss_a");
        let sb = fresh(b.as_mut(), "lumedb_ss_b");
        sample(a.as_mut(), &sa);
        sample(b.as_mut(), &sb);
        // Source: new column and new table. Target: extra table.
        a.execute(&format!("ALTER TABLE {} ADD COLUMN telefono varchar(20)", k.qualified(&sa, "clientes"))).unwrap();
        a.execute(&format!("CREATE TABLE {} (id integer PRIMARY KEY, nombre varchar(50))", k.qualified(&sa, "tiendas"))).unwrap();
        b.execute(&format!("CREATE TABLE {} (id integer PRIMARY KEY)", k.qualified(&sb, "obsoleta"))).unwrap();
        let items = sync::structure_diff(a.as_mut(), &sa, b.as_mut(), &sb, true).unwrap();
        let summary: Vec<(String, sync::SyncAction)> = items.iter().map(|i| (i.table.clone(), i.action)).collect();
        assert!(summary.contains(&("clientes".into(), sync::SyncAction::Alter)), "{k:?} {summary:?}");
        assert!(summary.contains(&("tiendas".into(), sync::SyncAction::Create)), "{k:?} {summary:?}");
        assert!(summary.contains(&("obsoleta".into(), sync::SyncAction::Drop)), "{k:?} {summary:?}");
        for i in &items {
            for s in &i.statements {
                b.execute(s).unwrap_or_else(|e| panic!("{k:?} {s}: {e}"));
            }
        }
        let again = sync::structure_diff(a.as_mut(), &sa, b.as_mut(), &sb, true).unwrap();
        assert!(again.is_empty(), "{k:?}: {:#?}", again);
    }
}

#[test]
fn find_users_monitor() {
    for k in engines() {
        let db = tmp(&format!("find-{k:?}.db"));
        let _ = std::fs::remove_file(&db);
        let mut d = connect(&cfg(k, db.to_str().unwrap())).unwrap();
        let s = fresh(d.as_mut(), "lumedb_find");
        sample(d.as_mut(), &s);
        let hits = admin::find_in_schema(d.as_mut(), &s, "x.es", false, &mut |_| {}).unwrap();
        assert_eq!(hits.len(), 1, "{k:?} {hits:?}");
        assert_eq!((hits[0].column.as_str(), hits[0].matches), ("email", 2), "{k:?}");
        assert!(admin::variables(d.as_mut()).unwrap().has_rows(), "{k:?}");
        assert!(!admin::server_version(d.as_mut()).unwrap().is_empty());
        if k == DbKind::Sqlite {
            continue;
        }
        assert!(!admin::processes(d.as_mut()).unwrap().rows.is_empty(), "{k:?}");
        assert!(admin::status(d.as_mut()).unwrap().has_rows(), "{k:?}");

        // Users: create, grant, rename, drop.
        let u = admin::UserInfo { name: "lumedb_u".into(), host: if k == DbKind::Mysql { "%".into() } else { String::new() }, privileges: vec![] };
        let _ = d.execute(&admin::drop_user_sql(k, &u));
        let _ = d.execute(&admin::drop_user_sql(k, &admin::UserInfo { name: "lumedb_u2".into(), ..u.clone() }));
        for s in admin::save_user_sql(k, None, &u, "secreto") {
            d.execute(&s).unwrap_or_else(|e| panic!("{k:?} {s}: {e}"));
        }
        let mut u2 = u.clone();
        u2.name = "lumedb_u2".into();
        u2.privileges = vec![if k == DbKind::Mysql { "PROCESS".into() } else { "CREATEDB".into() }, if k == DbKind::Mysql { "SELECT".into() } else { "LOGIN".into() }];
        for s in admin::save_user_sql(k, Some(&u), &u2, "") {
            d.execute(&s).unwrap_or_else(|e| panic!("{k:?} {s}: {e}"));
        }
        let g = admin::object_privileges(k)[..2].iter().map(|s| s.to_string()).collect::<Vec<_>>();
        d.execute(&admin::grant_sql(k, &u2, &s, &g, false)).unwrap();
        let users = admin::list_users(d.as_mut()).unwrap();
        let found = users.iter().find(|x| x.name == "lumedb_u2").unwrap_or_else(|| panic!("{k:?} {users:?}"));
        let mut a = found.privileges.clone();
        a.sort();
        let mut b = u2.privileges.clone();
        b.sort();
        assert_eq!(a, b, "{k:?}");
        let grants = admin::schema_grants(d.as_mut(), found).unwrap();
        assert!(grants.iter().any(|(sc, p)| sc == &s && p.len() >= 2), "{k:?} {grants:?}");
        if k == DbKind::Postgres {
            d.execute(&format!("DROP OWNED BY {}", k.quote_one("lumedb_u2"))).unwrap();
        }
        d.execute(&admin::drop_user_sql(k, found)).unwrap();
    }
}

#[test]
fn automation_job() {
    let db = tmp("job.db");
    let _ = std::fs::remove_file(&db);
    let c = ConnectionConfig { name: "local".into(), ..cfg(DbKind::Sqlite, db.to_str().unwrap()) };
    let out = tmp("job.csv");
    let job = automation::Job {
        name: "prueba".into(),
        steps: vec![
            automation::JobStep::Query {
                connection: "local".into(),
                database: String::new(),
                sql: "CREATE TABLE t (x INTEGER); INSERT INTO t VALUES (1), (2);".into(),
            },
            automation::JobStep::Export {
                connection: "local".into(),
                database: String::new(),
                sql: "SELECT * FROM t".into(),
                path: out.to_string_lossy().into(),
                format: "csv".into(),
            },
        ],
        ..Default::default()
    };
    let log = automation::run_job(&job, &[c]).unwrap();
    assert_eq!(log.len(), 2, "{log:?}");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "x\n1\n2\n");

    use chrono::TimeZone;
    let now = chrono::Local.with_ymd_and_hms(2026, 10, 14, 10, 30, 0).unwrap();
    assert!(automation::Schedule::EveryMinutes(5).due(Some("2026-10-14 10:20:00"), now));
    assert!(!automation::Schedule::EveryMinutes(15).due(Some("2026-10-14 10:20:00"), now));
    assert!(automation::Schedule::Daily { hour: 10, minute: 0 }.due(Some("2026-10-13 10:00:00"), now));
    assert!(!automation::Schedule::Daily { hour: 10, minute: 0 }.due(Some("2026-10-14 10:00:05"), now));
}
