//! Import of connections exported by Navicat (`.ncx`).
//!
//! Implemented from public documentation of the format and of the password
//! encryption; no Navicat code or binaries were used.

use std::collections::HashSet;

use aes::cipher::{block_padding::Pkcs7, BlockDecrypt, BlockDecryptMut, BlockEncrypt, BlockEncryptMut, KeyInit, KeyIvInit};
use anyhow::{anyhow, Context, Result};
use sha1::{Digest, Sha1};

use crate::config::{ConnectionConfig, DbKind, SshAuth, SshConfig};
use crate::{tr, trf};

#[derive(Debug, Default)]
pub struct Import {
    pub connections: Vec<ConnectionConfig>,
    /// Readable warnings: unsupported types, passwords that could not be decrypted, etc.
    pub warnings: Vec<String>,
}

pub fn parse_ncx(xml: &str) -> Result<Import> {
    let doc = roxmltree::Document::parse(xml).context(tr("the file is not valid XML"))?;
    let root = doc.root_element();
    if !root.has_tag_name("Connections") {
        return Err(anyhow!(tr("this does not look like an .ncx file (missing <Connections>)")));
    }

    let mut out = Import::default();
    for node in root.children().filter(|n| n.has_tag_name("Connection")) {
        let attr = |k: &str| node.attribute(k).unwrap_or("").trim().to_string();
        let name = attr("ConnectionName");
        let conn_type = attr("ConnType");

        let kind = match conn_type.to_ascii_uppercase().as_str() {
            "MYSQL" | "MARIADB" => DbKind::Mysql,
            "POSTGRESQL" | "POSTGRES" => DbKind::Postgres,
            "SQLITE" => DbKind::Sqlite,
            other => {
                out.warnings.push(trf!("\"{name}\": type {other} is not supported yet, skipped", name = name, other = other));
                continue;
            }
        };

        let mut password = String::new();
        let encrypted = attr("Password");
        if !encrypted.is_empty() && attr("SavePassword") != "false" {
            match decrypt_password(&encrypted) {
                Ok(p) => password = p,
                Err(_) => out.warnings.push(trf!("\"{name}\": could not decrypt the password", name = name)),
            }
        }
        let mut ssh = SshConfig::default();
        if attr("SSH").eq_ignore_ascii_case("true") {
            ssh.enabled = true;
            ssh.host = attr("SSH_Host");
            ssh.port = attr("SSH_Port").parse().unwrap_or(22);
            ssh.user = attr("SSH_UserName");
            if attr("SSH_AuthenMethod").eq_ignore_ascii_case("PUBLICKEY") {
                ssh.auth = SshAuth::PublicKey;
                ssh.key_path = attr("SSH_PrivateKey");
                ssh.passphrase = decrypt_password(&attr("SSH_Passphrase")).unwrap_or_default();
            } else {
                ssh.password = decrypt_password(&attr("SSH_Password")).unwrap_or_default();
            }
        }

        out.connections.push(ConnectionConfig {
            name,
            kind,
            host: attr("Host"),
            port: attr("Port").parse().unwrap_or(kind.default_port()),
            user: attr("UserName"),
            password,
            database: attr("Database"),
            path: attr("DatabaseFileName"),
            ssh,
            ..Default::default()
        });
    }
    Ok(out)
}

/// Adds the imported connections, renaming those that clash with existing names.
pub fn merge(existing: &mut Vec<ConnectionConfig>, imported: Vec<ConnectionConfig>) {
    let mut names: HashSet<String> = existing.iter().map(|c| c.name.clone()).collect();
    for mut c in imported {
        if names.contains(&c.name) {
            let base = c.name.clone();
            c.name = (2..).map(|n| format!("{base} ({n})")).find(|n| !names.contains(n)).unwrap();
        }
        names.insert(c.name.clone());
        existing.push(c);
    }
}

// ---------------------------------------------------------------- Password encryption

const AES_KEY: &[u8; 16] = b"libcckeylibcckey";
const AES_IV: &[u8; 16] = b"libcciv libcciv ";
const BLOWFISH_SEED: &[u8] = b"3DC5CA39";

/// Tries the Navicat 12+ scheme first and then the Navicat 11 one.
pub fn decrypt_password(hex_str: &str) -> Result<String> {
    let data = hex::decode(hex_str).context(tr("password is not hexadecimal"))?;
    if data.len() % 16 == 0 {
        if let Ok(p) = decrypt_v12(&data) {
            return Ok(p);
        }
    }
    decrypt_v11(&data)
}

fn decrypt_v12(data: &[u8]) -> Result<String> {
    let plain =
        cbc::Decryptor::<aes::Aes128>::new(AES_KEY.into(), AES_IV.into()).decrypt_padded_vec_mut::<Pkcs7>(data).map_err(|_| anyhow!(tr("invalid padding")))?;
    Ok(String::from_utf8(plain)?)
}

pub fn encrypt_v12(plain: &str) -> String {
    let data = cbc::Encryptor::<aes::Aes128>::new(AES_KEY.into(), AES_IV.into()).encrypt_padded_vec_mut::<Pkcs7>(plain.as_bytes());
    hex::encode_upper(data)
}

type Block8 = [u8; 8];

fn blowfish_v11() -> (blowfish::Blowfish, Block8) {
    let key = Sha1::digest(BLOWFISH_SEED);
    let bf = blowfish::Blowfish::new_from_slice(&key).expect("valid 20-byte key");
    let mut iv = [0xFFu8; 8];
    bf.encrypt_block((&mut iv).into());
    (bf, iv)
}

fn xor8(a: &mut Block8, b: &[u8]) {
    a.iter_mut().zip(b).for_each(|(x, y)| *x ^= y);
}

fn decrypt_v11(data: &[u8]) -> Result<String> {
    let (bf, mut cv) = blowfish_v11();
    let mut out = Vec::with_capacity(data.len());
    let (blocks, rest) = data.as_chunks::<8>();
    for chunk in blocks {
        let mut block: Block8 = *chunk;
        bf.decrypt_block((&mut block).into());
        xor8(&mut block, &cv);
        out.extend_from_slice(&block);
        xor8(&mut cv, chunk);
    }
    if !rest.is_empty() {
        bf.encrypt_block((&mut cv).into());
        out.extend(rest.iter().zip(cv).map(|(a, b)| a ^ b));
    }
    Ok(String::from_utf8(out)?)
}

pub fn encrypt_v11(plain: &str) -> String {
    let (bf, mut cv) = blowfish_v11();
    let data = plain.as_bytes();
    let mut out = Vec::with_capacity(data.len());
    let (blocks, rest) = data.as_chunks::<8>();
    for chunk in blocks {
        let mut block: Block8 = *chunk;
        xor8(&mut block, &cv);
        bf.encrypt_block((&mut block).into());
        xor8(&mut cv, &block);
        out.extend_from_slice(&block);
    }
    if !rest.is_empty() {
        bf.encrypt_block((&mut cv).into());
        out.extend(rest.iter().zip(cv).map(|(a, b)| a ^ b));
    }
    hex::encode_upper(out)
}
