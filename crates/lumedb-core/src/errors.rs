//! Turns raw driver errors into messages people can act on.
//!
//! Drivers report things like `DriverError { Could not connect to address `127.0.0.1:3308':
//! Connection refused (os error 111) }`. [`humanize`] recognises the common cases (server down,
//! wrong password, unknown host…) and returns a short sentence, a hint about what to check and the
//! cleaned technical text for a "Details" section.

use crate::{tr, trf};

#[derive(Debug, Clone, PartialEq)]
pub struct FriendlyError {
    /// One sentence saying what went wrong.
    pub summary: String,
    /// What the user can check or do about it.
    pub hint: Option<&'static str>,
    /// The technical text, cleaned up; empty when it would only repeat the summary.
    pub details: String,
}

pub fn humanize(raw: &str) -> FriendlyError {
    let details = clean(raw);
    let low = details.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| low.contains(w));
    let addr = address(&details);

    let known: Option<(String, &'static str)> = if has(&["connection refused"]) {
        Some((
            match &addr {
                Some(a) => trf!("The server at {addr} refused the connection.", addr = a),
                None => tr("The server refused the connection.").to_string(),
            },
            tr("Check that the database server is running and that the host and port are correct."),
        ))
    } else if has(&["timed out", "timeout"]) {
        Some((tr("The server did not respond in time.").into(), tr("Check the host and port, and any firewall or VPN in between.")))
    } else if has(&["failed to lookup address", "name or service not known", "no such host", "nodename nor servname", "could not translate host name"]) {
        Some((tr("The server's host name could not be found.").into(), tr("Check the host name in the connection settings.")))
    } else if has(&["no route to host", "network is unreachable"]) {
        Some((tr("The server cannot be reached from this network.").into(), tr("Check your network connection, VPN or SSH tunnel.")))
    } else if low.contains("ssh") && has(&["auth", "password", "key"]) {
        Some((tr("SSH authentication failed.").into(), tr("Check the SSH user name, password or private key.")))
    } else if has(&["access denied for user", "password authentication failed", "authentication failed", "no password supplied"]) {
        Some((tr("The user name or password is incorrect.").into(), tr("Check the credentials in the connection settings.")))
    } else if has(&["unknown database"]) || (low.contains("database") && low.contains("does not exist")) {
        Some((tr("The database does not exist.").into(), tr("Check the database name, or refresh the connection.")))
    } else if has(&["certificate", "handshake", "tls", "ssl"]) {
        Some((tr("The secure (SSL) connection could not be established.").into(), tr("Check the SSL settings of the connection and the server certificate.")))
    } else if has(&["too many connections"]) {
        Some((tr("The server has too many open connections.").into(), tr("Close some connections or try again later.")))
    } else if has(&["connection reset", "broken pipe", "server has gone away", "lost connection", "connection closed", "unexpected eof"]) {
        Some((tr("The connection to the server was lost.").into(), tr("Reconnect and try again.")))
    } else {
        None
    };

    match known {
        Some((summary, hint)) => FriendlyError { summary, hint: Some(hint), details },
        None => {
            // Unknown error: the cleaned text itself is the message.
            let first = details.lines().next().unwrap_or_default().trim().to_string();
            let summary = sentence(&first);
            let rest = if details.trim() == first { String::new() } else { details };
            FriendlyError { summary, hint: None, details: rest }
        }
    }
}

/// Removes driver noise: `DriverError { … }` wrappers, `(os error 111)`, `db error:` prefixes and
/// repeated lines from error chains.
pub fn clean(raw: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in raw.lines() {
        let mut l = line.trim().to_string();
        // `Name { text }` → `text` (mysql crate's error Display), possibly nested.
        while let Some(open) = l.find(" { ") {
            let name = &l[..open];
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || !l.ends_with('}') {
                break;
            }
            l = l[open + 3..l.len() - 1].trim().to_string();
        }
        if let Some(i) = l.find(" (os error ") {
            if let Some(end) = l[i..].find(')') {
                l.replace_range(i..i + end + 1, "");
            }
        }
        for prefix in ["db error: ", "error: ", "Error: "] {
            if let Some(rest) = l.strip_prefix(prefix) {
                l = rest.to_string();
            }
        }
        let l = l.replace(['`'], "'");
        if !l.is_empty() && !lines.iter().any(|p| p.contains(&l)) {
            lines.push(l);
        }
    }
    lines.join("\n")
}

/// `host:port` mentioned in the error, if any.
fn address(text: &str) -> Option<String> {
    let start = text.find("address '")? + "address '".len();
    let end = text[start..].find('\'')?;
    Some(text[start..start + end].to_string())
}

fn sentence(s: &str) -> String {
    let mut c = s.chars();
    let mut out = match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => return String::new(),
    };
    if !out.ends_with(['.', '!', '?', ':']) {
        out.push('.');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mysql_refused() {
        let f = humanize("DriverError { Could not connect to address `127.0.0.1:3308': Connection refused (os error 111) }");
        assert_eq!(f.summary, "The server at 127.0.0.1:3308 refused the connection.");
        assert!(f.hint.is_some());
        assert_eq!(f.details, "Could not connect to address '127.0.0.1:3308': Connection refused");
    }

    #[test]
    fn mysql_access_denied() {
        let f = humanize("MySqlError { ERROR 1045 (28000): Access denied for user 'x'@'172.17.0.1' (using password: YES) }");
        assert_eq!(f.summary, "The user name or password is incorrect.");
        assert!(f.details.starts_with("ERROR 1045"));
    }

    #[test]
    fn postgres_chain() {
        let f = humanize("error connecting to server: Connection refused (os error 111)\nConnection refused (os error 111)");
        assert_eq!(f.summary, "The server refused the connection.");
        assert_eq!(f.details, "error connecting to server: Connection refused");
    }

    #[test]
    fn unknown_error_is_kept() {
        let f = humanize("Table 'shop.orders' doesn't exist");
        assert_eq!(f.summary, "Table 'shop.orders' doesn't exist.");
        assert!(f.hint.is_none());
        assert!(f.details.is_empty());
    }
}
