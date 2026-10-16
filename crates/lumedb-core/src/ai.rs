//! SQL generation from natural language with an AI model (Claude or OpenAI).
//!
//! Rust has no official Anthropic SDK, so the HTTP API is used directly.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{config_dir, DbKind};
use crate::driver::Driver;
use crate::meta::{self, ObjectKind};
use crate::{tr, trf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AiProvider {
    #[default]
    Anthropic,
    OpenAi,
}

impl AiProvider {
    pub fn label(self) -> &'static str {
        match self {
            AiProvider::Anthropic => "Claude (Anthropic)",
            AiProvider::OpenAi => "OpenAI",
        }
    }
}

pub const DEFAULT_ANTHROPIC_MODEL: &str = "claude-opus-5-5";
pub const DEFAULT_OPENAI_MODEL: &str = "gpt-4.1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    pub provider: AiProvider,
    pub anthropic_key: String,
    pub anthropic_model: String,
    pub openai_key: String,
    pub openai_model: String,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: AiProvider::Anthropic,
            anthropic_key: String::new(),
            anthropic_model: DEFAULT_ANTHROPIC_MODEL.into(),
            openai_key: String::new(),
            openai_model: DEFAULT_OPENAI_MODEL.into(),
        }
    }
}

impl AiConfig {
    /// Key of the active provider (the saved one or, if empty, the environment variable).
    pub fn key(&self) -> Option<String> {
        let (saved, env) = match self.provider {
            AiProvider::Anthropic => (&self.anthropic_key, "ANTHROPIC_API_KEY"),
            AiProvider::OpenAi => (&self.openai_key, "OPENAI_API_KEY"),
        };
        if !saved.trim().is_empty() {
            return Some(saved.trim().to_string());
        }
        std::env::var(env).ok().filter(|k| !k.trim().is_empty())
    }

    pub fn model(&self) -> &str {
        match self.provider {
            AiProvider::Anthropic if !self.anthropic_model.trim().is_empty() => self.anthropic_model.trim(),
            AiProvider::Anthropic => DEFAULT_ANTHROPIC_MODEL,
            AiProvider::OpenAi if !self.openai_model.trim().is_empty() => self.openai_model.trim(),
            AiProvider::OpenAi => DEFAULT_OPENAI_MODEL,
        }
    }

    pub fn is_configured(&self) -> bool {
        self.key().is_some()
    }
}

fn path() -> Result<PathBuf> {
    Ok(config_dir()?.join("ai.json"))
}

const ANTHROPIC_SECRET: &str = "ai/anthropic";
const OPENAI_SECRET: &str = "ai/openai";

/// Loads the settings; the API keys come from the keyring. Keys still in plain text in the file
/// are moved to the keyring.
pub fn load() -> AiConfig {
    let mut cfg: AiConfig = path().ok().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let plain = !cfg.anthropic_key.is_empty() || !cfg.openai_key.is_empty();
    for (account, key) in [(ANTHROPIC_SECRET, &mut cfg.anthropic_key), (OPENAI_SECRET, &mut cfg.openai_key)] {
        if key.is_empty() {
            *key = crate::secrets::get(account).unwrap_or_default();
        }
    }
    if plain && crate::secrets::available() {
        let _ = save(&cfg);
    }
    cfg
}

/// Saves the settings; the API keys go to the keyring (or stay in the file, readable only by the
/// user, when there is no keyring).
pub fn save(cfg: &AiConfig) -> Result<()> {
    let mut out = cfg.clone();
    if crate::secrets::available() {
        crate::secrets::set(ANTHROPIC_SECRET, out.anthropic_key.trim())?;
        crate::secrets::set(OPENAI_SECRET, out.openai_key.trim())?;
        out.anthropic_key.clear();
        out.openai_key.clear();
    }
    crate::config::write_private(&path()?, &serde_json::to_string_pretty(&out)?)
}

// ---------------------------------------------------------------- Schema context

/// Compact schema description for the model: tables, columns, types and relationships.
pub fn describe_schema(d: &mut dyn Driver, schema: &str, max_tables: usize) -> Result<String> {
    let mut out = String::new();
    let objects = meta::list_objects(d, schema)?;
    let tables: Vec<_> = objects.iter().filter(|o| matches!(o.kind, ObjectKind::Table | ObjectKind::View)).take(max_tables).collect();
    for o in &tables {
        let cols = meta::columns(d, schema, &o.name).unwrap_or_default();
        let list: Vec<String> = cols
            .iter()
            .map(|c| {
                let mut s = format!("{} {}", c.name, c.data_type);
                if c.primary_key {
                    s.push_str(" PK");
                }
                if !c.nullable {
                    s.push_str(" NOT NULL");
                }
                s
            })
            .collect();
        let kind = if o.kind == ObjectKind::View { "VIEW" } else { "TABLE" };
        out.push_str(&format!("{kind} {}({})\n", o.name, list.join(", ")));
        if o.kind == ObjectKind::Table {
            for f in meta::foreign_keys(d, schema, &o.name).unwrap_or_default() {
                out.push_str(&format!("  FK {}.({}) -> {}.({})\n", o.name, f.columns.join(", "), f.ref_table, f.ref_columns.join(", ")));
            }
        }
    }
    if objects.len() > tables.len() {
        out.push_str(&format!("-- ({} more objects omitted)\n", objects.len() - tables.len()));
    }
    Ok(out)
}

fn system_prompt(dialect: DbKind) -> String {
    let name = match dialect {
        DbKind::Mysql => "MySQL 8 / MariaDB",
        DbKind::Postgres => "PostgreSQL",
        DbKind::Sqlite => "SQLite",
    };
    format!(
        "You write SQL for {name}. The user describes what they want in natural language (often Spanish) and you \
         answer with one SQL query that does it, using only the tables and columns in the schema provided. \
         Quote identifiers only when needed. Prefer readable formatting with one clause per line. \
         If the request is ambiguous, pick the most reasonable interpretation and note the assumption in a short \
         SQL comment at the top. Reply with only the SQL inside a single ```sql code block, without any other text."
    )
}

fn user_prompt(schema_name: &str, schema: &str, request: &str) -> String {
    format!("Database/schema: {schema_name}\n\n<schema>\n{schema}</schema>\n\nRequest: {request}")
}

/// Extracts the SQL from the ```sql block of the reply (or the whole reply if there is none).
pub fn extract_sql(text: &str) -> String {
    if let Some(start) = text.find("```") {
        let after = &text[start + 3..];
        let body_start = after.find('\n').map_or(0, |n| n + 1);
        let body = &after[body_start..];
        let end = body.find("```").unwrap_or(body.len());
        return body[..end].trim().to_string();
    }
    text.trim().to_string()
}

// ---------------------------------------------------------------- Requests

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(Duration::from_secs(180)).build()
}

/// Request body for the Anthropic Messages API.
pub fn anthropic_body(model: &str, system: &str, user: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "max_tokens": 16000,
        "system": system,
        // If the safety filters refuse the request, the server retries it with the recommended model.
        "fallbacks": "default",
        "output_config": { "effort": "medium" },
        "messages": [{ "role": "user", "content": user }],
    })
}

/// Text of the Anthropic reply (checking first whether it was refused).
pub fn anthropic_text(resp: &serde_json::Value) -> Result<String> {
    if resp["stop_reason"] == "refusal" {
        let why = resp["stop_details"]["explanation"].as_str().unwrap_or(tr("no details"));
        bail!(trf!("the model refused the request: {why}", why = why));
    }
    let text: String = resp["content"]
        .as_array()
        .map(|blocks| blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join(""))
        .unwrap_or_default();
    if text.trim().is_empty() {
        bail!(trf!("the reply contains no text (stop_reason: {reason})", reason = resp["stop_reason"]));
    }
    Ok(text)
}

pub fn openai_body(model: &str, system: &str, user: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
    })
}

pub fn openai_text(resp: &serde_json::Value) -> Result<String> {
    resp["choices"][0]["message"]["content"].as_str().map(str::to_string).ok_or_else(|| anyhow!(tr("OpenAI reply has no text")))
}

fn http_error(e: ureq::Error) -> anyhow::Error {
    match e {
        ureq::Error::Status(code, r) => {
            let body = r.into_string().unwrap_or_default();
            let msg = serde_json::from_str::<serde_json::Value>(&body).ok().and_then(|v| v["error"]["message"].as_str().map(str::to_string)).unwrap_or(body);
            match code {
                401 => anyhow!(trf!("invalid API key ({code}): {msg}", code = code, msg = msg)),
                429 => anyhow!(trf!("rate limit reached ({code}): {msg}", code = code, msg = msg)),
                _ => anyhow!(trf!("API error {code}: {msg}", code = code, msg = msg)),
            }
        }
        ureq::Error::Transport(t) => anyhow!(trf!("could not connect to the API: {error}", error = t)),
    }
}

/// Sends a request to the configured provider and returns the text of the reply.
pub fn complete(cfg: &AiConfig, system: &str, user: &str) -> Result<String> {
    let key = cfg.key().ok_or_else(|| anyhow!(trf!("no API key configured for {provider}", provider = cfg.provider.label())))?;
    match cfg.provider {
        AiProvider::Anthropic => {
            let resp: serde_json::Value = agent()
                .post("https://api.anthropic.com/v1/messages")
                .set("x-api-key", &key)
                .set("anthropic-version", "2023-06-01")
                .set("anthropic-beta", "server-side-fallback-2026-07-01")
                .set("content-type", "application/json")
                .send_json(anthropic_body(cfg.model(), system, user))
                .map_err(http_error)?
                .into_json()
                .context(tr("invalid response from Anthropic"))?;
            anthropic_text(&resp)
        }
        AiProvider::OpenAi => {
            let resp: serde_json::Value = agent()
                .post("https://api.openai.com/v1/chat/completions")
                .set("authorization", &format!("Bearer {key}"))
                .set("content-type", "application/json")
                .send_json(openai_body(cfg.model(), system, user))
                .map_err(http_error)?
                .into_json()
                .context(tr("invalid response from OpenAI"))?;
            openai_text(&resp)
        }
    }
}

/// Generates the SQL for `request` using the actual database schema.
pub fn generate_sql(cfg: &AiConfig, d: &mut dyn Driver, schema: &str, request: &str) -> Result<String> {
    let dialect = d.kind();
    let description = describe_schema(d, schema, 300)?;
    let text = complete(cfg, &system_prompt(dialect), &user_prompt(schema, &description, request))?;
    let sql = extract_sql(&text);
    if sql.is_empty() {
        bail!(tr("the AI returned no query"));
    }
    Ok(sql)
}

/// Quick check of the key and the model.
pub fn test(cfg: &AiConfig) -> Result<String> {
    let text = complete(cfg, "Reply with exactly: OK", "ping")?;
    Ok(trf!("{model} replied: {text}", model = cfg.model(), text = text.trim()))
}
