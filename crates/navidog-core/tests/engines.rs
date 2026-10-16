//! Same scenario on all three engines. PostgreSQL and MySQL only with `NAVIDOG_TEST_LIVE=1`
//! (test containers on ports 55432 and 33306, password `navidog`).

use navidog_core::ddl::{self, ColumnDef, IndexDef};
use navidog_core::edit::{change_sql, RowChange};
use navidog_core::{connect, meta, ConnectionConfig, DbKind, Driver, ObjectKind, Value};

fn live() -> bool {
    std::env::var("NAVIDOG_TEST_LIVE").is_ok()
}

fn open(kind: DbKind) -> (Box<dyn Driver>, String) {
    let mut cfg = ConnectionConfig { kind, host: "127.0.0.1".into(), password: "navidog".into(), ..Default::default() };
    match kind {
        DbKind::Sqlite => {
            cfg.path = ":memory:".into();
            (connect(&cfg).unwrap(), "main".into())
        }
        DbKind::Postgres => {
            cfg.port = 55432;
            cfg.user = "postgres".into();
            let mut d = connect(&cfg).unwrap();
            d.execute("DROP SCHEMA IF EXISTS navidog_test CASCADE; CREATE SCHEMA navidog_test").unwrap();
            (d, "navidog_test".into())
        }
        DbKind::Mysql => {
            cfg.port = 33306;
            cfg.user = "root".into();
            let mut d = connect(&cfg).unwrap();
            d.execute("DROP DATABASE IF EXISTS navidog_test").unwrap();
            d.execute("CREATE DATABASE navidog_test").unwrap();
            (d, "navidog_test".into())
        }
    }
}

fn col(name: &str, ty: &str) -> ColumnDef {
    ColumnDef { name: name.into(), data_type: ty.into(), nullable: true, ..Default::default() }
}

fn run(kind: DbKind) {
    let (mut d, schema) = open(kind);
    let d = d.as_mut();
    let k = kind;
    let int = if k == DbKind::Sqlite { "INTEGER" } else { "integer" };
    let text = match k {
        DbKind::Postgres => "character varying(100)",
        DbKind::Mysql => "varchar(100)",
        DbKind::Sqlite => "TEXT",
    };

    // --- CREATE with the DDL generator
    let mut perros = ddl::TableDesign {
        schema: schema.clone(),
        name: "perros".into(),
        columns: vec![
            ColumnDef { primary_key: true, auto_increment: true, nullable: false, ..col("id", int) },
            ColumnDef { nullable: false, ..col("nombre", text) },
            ColumnDef { default: "0".into(), ..col("edad", int) },
        ],
        indexes: vec![IndexDef { name: "idx_perros_nombre".into(), columns: vec!["nombre".into()], ..Default::default() }],
        comment: if k == DbKind::Sqlite { String::new() } else { "Perros de la tienda".into() },
        ..Default::default()
    };
    for s in ddl::create_table(k, &perros) {
        d.execute(&s).unwrap_or_else(|e| panic!("{k:?}: {s}\n{e}"));
    }
    let vacunas = ddl::TableDesign {
        schema: schema.clone(),
        name: "vacunas".into(),
        columns: vec![
            ColumnDef { primary_key: true, nullable: false, ..col("id", int) },
            col("perro_id", int),
            col("datos", if k == DbKind::Postgres { "bytea" } else { "blob" }),
        ],
        foreign_keys: vec![ddl::ForeignKeyDef {
            name: "fk_vacunas_perro".into(),
            columns: vec!["perro_id".into()],
            ref_schema: schema.clone(),
            ref_table: "perros".into(),
            ref_columns: vec!["id".into()],
            on_delete: "CASCADE".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    for s in ddl::create_table(k, &vacunas) {
        d.execute(&s).unwrap_or_else(|e| panic!("{k:?}: {s}\n{e}"));
    }
    d.execute(&format!("CREATE VIEW {} AS SELECT nombre FROM {}", k.qualified(&schema, "nombres"), k.qualified(&schema, "perros")))
        .unwrap();

    // --- Metadata
    assert!(meta::list_schemas(d).unwrap().contains(&schema), "{k:?}");
    let objs = meta::list_objects(d, &schema).unwrap();
    let names: Vec<(&str, ObjectKind)> = objs.iter().map(|o| (o.name.as_str(), o.kind)).collect();
    assert!(names.contains(&("perros", ObjectKind::Table)), "{k:?} {names:?}");
    assert!(names.contains(&("nombres", ObjectKind::View)), "{k:?} {names:?}");

    let design = meta::table_design(d, &schema, "perros").unwrap();
    let cols: Vec<&str> = design.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(cols, ["id", "nombre", "edad"], "{k:?}");
    assert!(design.columns[0].primary_key && design.columns[0].auto_increment, "{k:?} {:?}", design.columns[0]);
    assert!(!design.columns[1].nullable, "{k:?}");
    assert_eq!(design.columns[2].default, "0", "{k:?}");
    assert_eq!(design.indexes.len(), 1, "{k:?} {:?}", design.indexes);
    assert_eq!(design.indexes[0].columns, ["nombre"], "{k:?}");
    assert_eq!(design.comment, perros.comment, "{k:?}");

    let fks = meta::foreign_keys(d, &schema, "vacunas").unwrap();
    assert_eq!(fks.len(), 1, "{k:?}");
    assert_eq!((fks[0].ref_table.as_str(), fks[0].on_delete.as_str()), ("perros", "CASCADE"), "{k:?}");

    for o in &objs {
        let ddl = meta::object_ddl(d, o).unwrap_or_else(|e| panic!("{k:?} {o:?}: {e}"));
        assert!(ddl.to_uppercase().contains("CREATE"), "{k:?}: {ddl}");
    }

    // --- Data editing
    let ins = |nombre: &str| RowChange::Insert { values: vec![("nombre".into(), Value::Text(nombre.into()))] };
    for n in ["Toby", "Luna", "O'Malley \\ barra"] {
        d.execute(&change_sql(k, &schema, "perros", &ins(n), false)).unwrap();
    }
    let up = RowChange::Update {
        key: vec![("id".into(), Value::Number("2".into()))],
        set: vec![("edad".into(), Value::Number("7".into()))],
    };
    assert_eq!(d.execute(&change_sql(k, &schema, "perros", &up, false)).unwrap().affected, Some(1), "{k:?}");
    let del = RowChange::Delete { key: vec![("id".into(), Value::Number("1".into()))] };
    d.execute(&change_sql(k, &schema, "perros", &del, false)).unwrap();
    let r = d.execute(&format!("SELECT id, nombre, edad FROM {} ORDER BY id", k.qualified(&schema, "perros"))).unwrap();
    assert_eq!(r.rows.len(), 2, "{k:?}");
    assert_eq!(r.rows[0][1], Value::Text("Luna".into()), "{k:?}");
    assert_eq!(r.rows[1][1], Value::Text("O'Malley \\ barra".into()), "{k:?}");
    if k != DbKind::Postgres {
        assert_eq!(r.rows[0][2], Value::Number("7".into()), "{k:?}");
    }

    // Binary data
    let blob = Value::Bytes(vec![0, 1, 2, 255]);
    d.execute(&format!(
        "INSERT INTO {} (id, perro_id, datos) VALUES (1, 2, {})",
        k.qualified(&schema, "vacunas"),
        blob.sql_literal(k)
    ))
    .unwrap();
    let r = d.execute(&format!("SELECT datos FROM {}", k.qualified(&schema, "vacunas"))).unwrap();
    match k {
        DbKind::Postgres => assert_eq!(r.rows[0][0], Value::Text("\\x000102ff".into())),
        _ => assert_eq!(r.rows[0][0], blob, "{k:?}"),
    }

    // --- ALTER: rename, change type, add, drop and index
    perros = design.clone();
    perros.columns[1].name = "nombre_completo".into();
    perros.columns[2].data_type = if k == DbKind::Sqlite { "REAL".into() } else { "bigint".into() };
    perros.columns.push(col("raza", text));
    perros.indexes[0].columns = vec!["nombre_completo".into()];
    perros.indexes.push(IndexDef { name: "idx_raza".into(), columns: vec!["raza".into()], unique: true, ..Default::default() });
    let stmts = ddl::alter_table(k, &design, &perros);
    assert!(!stmts.is_empty());
    for s in &stmts {
        d.execute(s).unwrap_or_else(|e| panic!("{k:?}: {s}\n{e}\n{stmts:#?}"));
    }
    let after = meta::table_design(d, &schema, "perros").unwrap();
    let cols: Vec<&str> = after.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(cols, ["id", "nombre_completo", "edad", "raza"], "{k:?}");
    assert_eq!(after.indexes.len(), 2, "{k:?} {:?}", after.indexes);
    assert!(after.columns[0].primary_key, "{k:?}");
    let r = d.execute(&format!("SELECT COUNT(*) FROM {}", k.qualified(&schema, "perros"))).unwrap();
    assert_eq!(r.rows[0][0].to_text().as_deref(), Some("2"), "data survives the rebuild ({k:?})");

    // No changes, no statements.
    assert!(ddl::alter_table(k, &after, &after).is_empty(), "{k:?}");

    // Drop a column
    let mut fewer = after.clone();
    fewer.columns.retain(|c| c.name != "edad");
    for s in ddl::alter_table(k, &after, &fewer) {
        d.execute(&s).unwrap_or_else(|e| panic!("{k:?}: {s}\n{e}"));
    }
    assert_eq!(meta::columns(d, &schema, "perros").unwrap().len(), 3, "{k:?}");

    assert!(d.canceller().is_some());
}

#[test]
fn sqlite() {
    run(DbKind::Sqlite);
}

#[test]
fn postgres() {
    if live() {
        run(DbKind::Postgres);
    }
}

#[test]
fn mysql() {
    if live() {
        run(DbKind::Mysql);
    }
}
