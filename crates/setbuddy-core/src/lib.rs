//! Setbuddy's cross-platform core: library index, queue, resume, and the player
//! facade that the CLI and the macOS app both drive.
//!
//! Nothing here knows which playback engine is in use. Everything routes through
//! the [`setbuddy_engine::PlaybackEngine`] contract, selected per file by
//! [`selection::EngineRegistry`].

pub mod artwork;
pub mod error;
pub mod library;
pub mod paths;
pub mod player;
pub mod probe;
pub mod queue;
pub mod resume;
pub mod selection;
pub mod store;
pub mod track;

pub use error::{CoreError, Result};
pub use player::{Player, PlayerStatus};
pub use queue::{Queue, RepeatMode};
pub use selection::{EnginePolicy, EngineRegistry};
pub use store::Store;
pub use track::{format_duration, Track};
