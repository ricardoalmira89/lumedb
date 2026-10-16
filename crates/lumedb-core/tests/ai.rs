use lumedb_core::ai::{anthropic_body, anthropic_text, describe_schema, extract_sql, openai_text, AiConfig, AiProvider};
use lumedb_core::{connect, ConnectionConfig, DbKind};

#[test]
fn extracts_sql_from_code_block() {
    let text = "Aquí tienes:\n```sql\nSELECT DATE(created_at) AS dia, COUNT(*)\nFROM request\nGROUP BY 1;\n```\nListo.";
    assert_eq!(extract_sql(text), "SELECT DATE(created_at) AS dia, COUNT(*)\nFROM request\nGROUP BY 1;");
    assert_eq!(extract_sql("SELECT 1;"), "SELECT 1;");
}

#[test]
fn anthropic_request_and_response_shapes() {
    let body = anthropic_body("claude-opus-5-5", "sys", "hola");
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["fallbacks"], "default");
    assert_eq!(body["messages"][0]["role"], "user");
    assert!(body.get("thinking").is_none(), "Opus 5.5 rejects disabled thinking; it is omitted");

    let ok = serde_json::json!({"stop_reason": "end_turn", "content": [
        {"type": "thinking", "thinking": ""},
        {"type": "text", "text": "```sql\nSELECT 1;\n```"}
    ]});
    assert_eq!(extract_sql(&anthropic_text(&ok).unwrap()), "SELECT 1;");
    let refused = serde_json::json!({"stop_reason": "refusal", "content": [], "stop_details": {"type": "refusal", "category": null, "explanation": "x"}});
    assert!(anthropic_text(&refused).is_err());
}

#[test]
fn openai_response_shape() {
    let r = serde_json::json!({"choices": [{"message": {"role": "assistant", "content": "SELECT 2;"}}]});
    assert_eq!(openai_text(&r).unwrap(), "SELECT 2;");
}

#[test]
fn key_falls_back_to_saved_then_env() {
    let mut c = AiConfig { provider: AiProvider::OpenAi, openai_key: "  sk-test  ".into(), ..Default::default() };
    assert_eq!(c.key().as_deref(), Some("sk-test"));
    c.openai_model.clear();
    assert_eq!(c.model(), lumedb_core::ai::DEFAULT_OPENAI_MODEL);
}

#[test]
fn schema_description_includes_columns_and_fks() {
    let mut d = connect(&ConnectionConfig { kind: DbKind::Sqlite, path: ":memory:".into(), ..Default::default() }).unwrap();
    d.execute("CREATE TABLE clientes (id INTEGER PRIMARY KEY, nombre TEXT NOT NULL)").unwrap();
    d.execute("CREATE TABLE pedidos (id INTEGER PRIMARY KEY, cliente_id INTEGER REFERENCES clientes(id), total REAL)").unwrap();
    let s = describe_schema(d.as_mut(), "main", 100).unwrap();
    assert!(s.contains("TABLE clientes(id INTEGER PK, nombre TEXT NOT NULL)"), "{s}");
    assert!(s.contains("FK pedidos.(cliente_id) -> clientes.(id)"), "{s}");
}

