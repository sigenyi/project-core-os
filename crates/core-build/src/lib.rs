//! core-build: builds C.O.R.E. OS from pinned sources.
//!
//! Recipes (`os/bootstrap/*.toml`, `os/recipes/*.toml`) are built in the order their
//! directory's `ORDER` file lists. The bootstrap builds a cross toolchain and the
//! temporary tools needed to enter the new root; every package of the real system is
//! then built inside that root, packaged as a `.cpk` and installed with cpkg, so the
//! final image can be assembled from packages alone.

pub mod builder;
pub mod env;
pub mod post;
pub mod prune;
pub mod recipe;
pub mod source;
