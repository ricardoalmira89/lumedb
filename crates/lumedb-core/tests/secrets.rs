//! Secrets in the system keyring. Needs an unlocked keyring, so it only runs with
//! `LUMEDB_TEST_KEYRING=1`; it uses a temporary config folder and removes its entries afterwards.

use lumedb_core::config::{load_connections, save_connections};
use lumedb_core::{secrets, ConnectionConfig, DbKind};

#[test]
fn passwords_move_to_the_keyring() {
    if std::env::var("LUMEDB_TEST_KEYRING").as_deref() != Ok("1") {
        eprintln!("skipped: set LUMEDB_TEST_KEYRING=1");
        return;
    }
    let dir = std::env::temp_dir().join(format!("lumedb-secrets-{}", std::process::id()));
    std::env::set_var("XDG_CONFIG_HOME", &dir);
    let file = dir.join("lumedb/connections.json");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    assert!(secrets::available(), "no keyring available");

    // A file from an older version, with the password in plain text.
    let old = ConnectionConfig { id: "test-secrets".into(), name: "t".into(), kind: DbKind::Mysql, password: "s3cret".into(), ..Default::default() };
    std::fs::write(&file, serde_json::to_string(&[&old]).unwrap()).unwrap();

    let loaded = load_connections().unwrap();
    assert_eq!(loaded[0].password, "s3cret");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.contains("s3cret"), "password still in the file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert_eq!(secrets::get("conn/test-secrets/password").as_deref(), Some("s3cret"));
    assert_eq!(load_connections().unwrap()[0].password, "s3cret");

    // Not saving the password removes it from the keyring.
    let mut c = loaded[0].clone();
    c.save_password = false;
    save_connections(&[c]).unwrap();
    assert_eq!(secrets::get("conn/test-secrets/password"), None);
    assert_eq!(load_connections().unwrap()[0].password, "");

    save_connections(&[]).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
