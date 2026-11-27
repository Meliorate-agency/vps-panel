//! Server snapshots: one encrypted file holding everything this server
//! hosts, which a freshly installed panel on another server imports.
//!
//! - `format`: names, manifest / index types, the age layer.
//! - `plan`: what a snapshot holds (a fixed list), and the walk over it.
//! - `archive`: the streaming writer and reader (tar -> zstd -> age).
//! - `dumps`, `inventory`: databases and facts about the server, gathered
//!   before the archive is written.
//! - `export`: the export task, the size estimate, the startup clean-up.
//! - `http_range`: serving the file with resumable ranges.

// Parts of the format are only read by the import, and rustc 1.95 panics
// while reporting dead code in this module (an ICE in check_mod_deathness).
#![allow(dead_code)]

pub mod archive;
pub mod dumps;
pub mod export;
pub mod format;
pub mod http_range;
pub mod import;
pub mod inventory;
pub mod merge;
pub mod plan;
pub mod pull;
