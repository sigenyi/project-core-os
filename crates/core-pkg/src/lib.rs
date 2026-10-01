//! cpkg: the C.O.R.E. OS package manager.
//!
//! Built for an operating system run by an AI as much as by people: every package
//! records what it provides (programs, libraries, services, configuration, manual
//! pages) and how it is used, every query can answer in JSON, and dependencies on
//! shared libraries are derived from the binaries themselves, so a repository can be
//! proven complete before anything is installed from it.
//!
//! * [`manifest`]: package metadata and file lists
//! * [`archive`]: the `.cpk` file format
//! * [`repo`]: signed repository indexes
//! * [`db`]: the installed-package database
//! * [`resolve`]: dependency resolution
//! * [`transaction`]: staged, verified installation and removal
//! * [`hooks`]: post-transaction triggers shipped by packages

pub mod archive;
pub mod db;
pub mod elf;
pub mod hooks;
pub mod manifest;
pub mod repo;
pub mod resolve;
pub mod transaction;
pub mod version;

pub use archive::create_package;
pub use db::Db;
pub use manifest::Manifest;
pub use repo::{Index, Repository};
