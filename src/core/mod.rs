//! Everything the features share: configuration, the database, the `Feature` trait, the
//! dispatcher that routes Discord events to features, and logging.
//!
//! Features may use anything in here. Nothing in here knows about a specific feature.

pub mod config;
pub mod ctx;
pub mod db;
pub mod dispatcher;
pub mod errors;
pub mod events;
pub mod feature;
pub mod gateway;
pub mod interaction;
pub mod legacy;
pub mod logging;
pub mod settings;
pub mod timers;

pub use ctx::BotCtx;
pub use errors::user_error;
pub use events::BotEvent;
pub use feature::{Asker, Feature, LegacyImport, Mention, Panel, Stat};
pub use interaction::{
    Command, CommandCtx, ComponentInteraction, ModalInteraction, Respond, Response,
};
pub use timers::Timers;

/// The error type used everywhere. `anyhow` keeps the chain of causes, so a log line reads
/// "sending reminder 12: Missing Access" instead of only "Missing Access".
pub type Error = anyhow::Error;
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The crate version from Cargo.toml.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The git commit the binary was built from (set by `build.rs`).
pub const GIT_COMMIT: &str = env!("GIT_COMMIT");
