//! LumeDB core: connections, drivers, metadata and SQL generation.

pub mod admin;
pub mod ai;
pub mod automation;
pub mod config;
pub mod ddl;
pub mod driver;
pub mod dump;
pub mod edit;
pub mod errors;
pub mod i18n;
pub mod meta;
pub mod ncx;
pub mod net;
pub mod secrets;
pub mod sql;
pub mod store;
pub mod sync;
pub mod transfer;

pub use config::{ConnectionConfig, DbKind};
pub use driver::{connect, Canceller, Driver, QueryResult, Value};
pub use i18n::{tr, trc};
pub use meta::{DbObject, ObjectKind};
