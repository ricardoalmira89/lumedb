# LumeDB

**English** · [Español](README.es.md)

A graphical database client written in Rust, with the layout and workflow of Navicat.
Engines: **PostgreSQL**, **MySQL / MariaDB** and **SQLite**.

## Clean-room rules (mandatory)

LumeDB is an independent implementation. To keep it that way:

1. **Navicat is never decompiled or disassembled**, and none of its code, resources (icons, texts, translations) or brand are copied.
2. **Using Navicat normally and observing its behaviour** is allowed (Directive 2009/24/EC, art. 5.3), as is reimplementing features, which are not protected by copyright (CJEU C-406/10, *SAS Institute*).
3. Interoperability with Navicat files (e.g. importing `.ncx` connections) is implemented from **public documentation** and sample files of our own, never from the binary.

## Features

**Connections**
- PostgreSQL (one connection per database, like Navicat), MySQL/MariaDB and SQLite.
- SSL (custom CA, optional verification) and SSH tunnel (password or public key).
- Groups (folders) and colours; duplicate, edit and test connections.
- Import Navicat connections (`.ncx`), including passwords and SSH.

**Navigation**
- Tree connection → database → schema → tables, views, functions, procedures, triggers, sequences, events, saved queries and backups; object search.
- "Objects" tab with rows, size, engine, dates and comment; info pane with General and DDL tabs.
- ER diagram view with minimap, auto-layout without crossing relations, and curved column-to-column links.

**Data**
- Editable grid: edit cells, add/delete rows, NULL / empty string, long-text editor, apply or discard changes.
- Manual transactions (begin, commit, rollback), WHERE filter, sort by column, pagination with total count, form view.

**Queries**
- Syntax highlighting, line numbers, autocompletion (keywords, tables, columns, `alias.column`).
- Run all, the selection or the current statement; stop a query; multiple results; Explain; format.
- Charts (bar, line, area, scatter, pie), visual query builder, code snippets.
- Saved queries, history and result export.
- AI SQL generation (Claude or OpenAI): write a comment and press Ctrl+Enter.

**Object design**
- Table designer: fields, indexes, foreign keys, options and SQL preview (CREATE/ALTER; rebuild on SQLite).
- Editor for views, functions, procedures, triggers, events and sequences, with per-engine templates.

**Tools**
- Import (CSV, TXT, JSON, Excel) with column mapping; export (CSV, TXT, JSON, XML, HTML, Markdown, SQL, Excel).
- Dump to SQL and run SQL files; compressed backups with restore.
- Data transfer between connections (also across engines), data and structure synchronization.
- Users and privileges (server and per database/schema), server monitor (processes, variables, status).
- Find in database, test data generation, ER model, console, scheduled automation.
- English and Spanish interface (View → Language); light and dark theme.

## Differences from Navicat (to do)

- Missing engines: SQL Server, Oracle, MongoDB, Redis and others.
- The ER model is reverse-engineered (diagram of the existing database + SQL export); it is not a full logical/physical designer.
- Automation runs while LumeDB is open or from cron (`lumedb --run-job "name"`); it does not send email.
- No function debugger, reports or cloud sync.

## Passwords

Connection passwords, SSH secrets and AI API keys are stored in the system keyring (GNOME Keyring /
KWallet via Secret Service on Linux, Keychain on macOS, Credential Manager on Windows), never in the
configuration files. Each connection has a "Save password" option; when it is off, LumeDB asks for
the password when connecting. Without a keyring (or with `LUMEDB_NO_KEYRING=1`) saved secrets stay
in the files, which are created readable only by your user (0600). Building on Linux needs
`libdbus-1-dev` (Debian/Ubuntu) or `dbus-devel` (Fedora).

## Usage

```sh
cargo run --release
cargo test                        # SQLite
LUMEDB_TEST_LIVE=1 cargo test    # also PostgreSQL (port 55432) and MySQL (33306), password "lumedb"
LUMEDB_TEST_KEYRING=1 cargo test # also the system keyring (needs an unlocked keyring)
```

Containers for the live tests:

```sh
docker run -d --name lumedb-test-pg -e POSTGRES_PASSWORD=lumedb -p 127.0.0.1:55432:5432 postgres:16-alpine
docker run -d --name lumedb-test-mysql -e MYSQL_ROOT_PASSWORD=lumedb -p 127.0.0.1:33306:3306 mysql:latest
```

## Translations

English is the source language: every interface string is written in English and wrapped in `tr("…")`
(or `trf!("… {name} …", name = value)`). Translations live in `crates/lumedb-core/i18n/<lang>.json`,
a map from the English text to the translated text; missing entries fall back to English.
To add a language, add a catalog and a variant to `Lang` in `crates/lumedb-core/src/i18n.rs`.

## Layout

- `crates/lumedb-core`: connections (SSL/SSH), drivers, metadata, DDL, editing, SQL, export/import, dump, synchronization, administration, automation and translations.
- `crates/lumedb-app`: user interface (egui). `app.rs` dispatches tasks to one thread per connection; every tab or window is a `View` in `views/`.
