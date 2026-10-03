//! The contract between C.O.R.E.'s untrusted reasoning layer and its trusted executor.
//!
//! The language model never runs commands. It emits an [`Intent`] (a JSON object naming
//! one action from the [`catalog`]). That intent is validated into a typed
//! [`ValidatedAction`], authorised by policy, and only then executed by the Guardian.
//!
//! This crate is dependency-light and shared by every other component, so the agent,
//! the Guardian and the grammar always agree on what an action is.

pub mod action;
pub mod catalog;
pub mod choice;
pub mod contract;
pub mod grammar;
pub mod intent;
pub mod logging;
pub mod paths;
pub mod risk;
pub mod time;
pub mod validate;
pub mod wire;

pub use action::{Action, ValidatedAction, ValidationError};
pub use catalog::{ActionSpec, CATALOG, Category, Executor, ParamKind, ParamSpec};
pub use intent::Intent;
pub use risk::Risk;

/// Version of the Agent ⇄ Guardian wire protocol.
pub const PROTOCOL_VERSION: u32 = 1;

/// Default location of the Guardian's socket.
pub const DEFAULT_GUARDIAN_SOCKET: &str = "/run/core/guardian.sock";

/// Default location of the telemetry snapshot written by `core-sensed`.
pub const DEFAULT_TELEMETRY_PATH: &str = "/run/core-sense/telemetry.json";
