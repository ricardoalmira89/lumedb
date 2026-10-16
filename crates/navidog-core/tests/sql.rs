use navidog_core::sql::{split_statements, statement_at};
use navidog_core::DbKind;

fn split(sql: &str, k: DbKind) -> Vec<String> {
    split_statements(sql, k).into_iter().map(|s| s.sql).collect()
}

#[test]
fn basic() {
    let s = "SELECT 1; -- comentario; con punto y coma\nSELECT 'a;b' ; /* ; */ SELECT \"x;\"";
    assert_eq!(split(s, DbKind::Postgres), ["SELECT 1", "SELECT 'a;b'", "SELECT \"x;\""]);
}

#[test]
fn dollar_quotes() {
    let s = "CREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql; SELECT f();";
    assert_eq!(split(s, DbKind::Postgres).len(), 2);
}

#[test]
fn mysql_delimiter() {
    let s = "DELIMITER ;;\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END;;\nDELIMITER ;\nCALL p();";
    let v = split(s, DbKind::Mysql);
    assert_eq!(v, ["CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END", "CALL p()"]);
}

#[test]
fn sqlite_trigger() {
    let s = "CREATE TRIGGER t AFTER INSERT ON a BEGIN UPDATE b SET x = CASE WHEN 1 THEN 2 END; DELETE FROM c; END; SELECT 1;";
    let v = split(s, DbKind::Sqlite);
    assert_eq!(v.len(), 2, "{v:?}");
}

#[test]
fn mysql_backslash() {
    assert_eq!(split(r"SELECT 'a\';b'; SELECT 2", DbKind::Mysql).len(), 2);
}

#[test]
fn cursor() {
    let s = "SELECT 1;\nSELECT 2;\nSELECT 3";
    assert_eq!(statement_at(s, DbKind::Sqlite, 12).unwrap().sql, "SELECT 2");
}
