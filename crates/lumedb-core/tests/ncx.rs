use lumedb_core::ncx::{decrypt_password, encrypt_v11, encrypt_v12, merge, parse_ncx};
use lumedb_core::{ConnectionConfig, DbKind};

#[test]
fn known_vectors() {
    // Vectors published in the public documentation of Navicat's encryption.
    assert_eq!(encrypt_v12("This is a test"), "B75D320B6211468D63EB3B67C9E85933");
    assert_eq!(encrypt_v11("This is a test"), "0EA71F51DD37BFB60CCBA219BE3A");
    assert_eq!(decrypt_password("B75D320B6211468D63EB3B67C9E85933").unwrap(), "This is a test");
    assert_eq!(decrypt_password("0EA71F51DD37BFB60CCBA219BE3A").unwrap(), "This is a test");
}

#[test]
fn roundtrip() {
    for p in ["", "a", "12345678", "contraseña-larga-ñ€", "exactamente16byt"] {
        assert_eq!(decrypt_password(&encrypt_v12(p)).unwrap_or_default(), p);
        assert_eq!(decrypt_password(&encrypt_v11(p)).unwrap_or_default(), p);
    }
}

#[test]
fn parse_file() {
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Connections Ver="1.5">
  <Connection ConnectionName="prod-pg" ConnType="POSTGRESQL" Host="db.local" Port="5433"
     Database="app" UserName="admin" Password="{}" SavePassword="true" SSH="false"/>
  <Connection ConnectionName="maria" ConnType="MARIADB" Host="127.0.0.1" Port="3306"
     UserName="root" Password="" SavePassword="false" SSH="true"/>
  <Connection ConnectionName="local" ConnType="SQLITE" DatabaseFileName="/tmp/x.db"/>
  <Connection ConnectionName="ora" ConnType="ORACLE" Host="o" Port="1521"/>
</Connections>"#,
        encrypt_v12("s3cr3t")
    );
    let imp = parse_ncx(&xml).unwrap();
    assert_eq!(imp.connections.len(), 3);

    let pg = &imp.connections[0];
    assert_eq!((pg.kind, pg.host.as_str(), pg.port), (DbKind::Postgres, "db.local", 5433));
    assert_eq!((pg.user.as_str(), pg.password.as_str(), pg.database.as_str()), ("admin", "s3cr3t", "app"));
    assert_eq!(imp.connections[1].kind, DbKind::Mysql);
    assert_eq!(imp.connections[2].path, "/tmp/x.db");
    assert_eq!(imp.warnings.len(), 1, "{:?}", imp.warnings); // Oracle
    assert!(imp.connections[1].ssh.enabled);
}

#[test]
fn merge_renames_duplicates() {
    let c = |n: &str| ConnectionConfig { name: n.into(), ..Default::default() };
    let mut existing = vec![c("a"), c("a (2)")];
    merge(&mut existing, vec![c("a"), c("b")]);
    let names: Vec<_> = existing.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["a", "a (2)", "a (3)", "b"]);
}
