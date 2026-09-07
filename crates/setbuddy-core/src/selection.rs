//! Choosing an engine for a file.
//!
//! Selection asks engines what they can *do*, never who they *are*. That is the
//! whole point of the boundary: when an AVFoundation engine is added it becomes
//! another entry in the registry with narrower `containers` and `native_pip`
//! set, and this code does not change.

use setbuddy_engine::{EngineError, SharedEngine};

use crate::error::{CoreError, Result};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum EnginePolicy {
    /// Use the first registered engine that can open the file. Registration
    /// order is the user's preference order.
    #[default]
    Auto,
    /// Always use this engine, by id. Reports the file as unsupported rather
    /// than quietly falling back, so an explicit choice stays explicit.
    Force(String),
}

impl EnginePolicy {
    pub fn as_str(&self) -> String {
        match self {
            EnginePolicy::Auto => "auto".into(),
            EnginePolicy::Force(id) => id.clone(),
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => EnginePolicy::Auto,
            other => EnginePolicy::Force(other.to_string()),
        }
    }
}

/// The engines available to this build, in preference order.
pub struct EngineRegistry {
    engines: Vec<SharedEngine>,
    policy: EnginePolicy,
}

impl EngineRegistry {
    pub fn new(engines: Vec<SharedEngine>) -> Self {
        Self {
            engines,
            policy: EnginePolicy::Auto,
        }
    }

    pub fn with_policy(mut self, policy: EnginePolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn policy(&self) -> &EnginePolicy {
        &self.policy
    }

    pub fn set_policy(&mut self, policy: EnginePolicy) {
        self.policy = policy;
    }

    pub fn ids(&self) -> Vec<String> {
        self.engines.iter().map(|e| e.capabilities().id).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.engines.is_empty()
    }

    pub fn by_id(&self, id: &str) -> Option<SharedEngine> {
        self.engines
            .iter()
            .find(|e| e.capabilities().id == id)
            .cloned()
    }

    /// The engine that should play `path`.
    pub fn select_for(&self, path: &str) -> Result<SharedEngine> {
        if self.engines.is_empty() {
            return Err(CoreError::Internal {
                message: "no playback engines are registered".into(),
            });
        }
        match &self.policy {
            EnginePolicy::Auto => self
                .engines
                .iter()
                .find(|e| e.capabilities().handles_path(path))
                .cloned()
                .ok_or_else(|| {
                    CoreError::Engine(EngineError::Unsupported {
                        path: path.to_string(),
                    })
                }),
            EnginePolicy::Force(id) => {
                let engine = self.by_id(id).ok_or_else(|| CoreError::Internal {
                    message: format!("no engine with id \"{id}\" is available"),
                })?;
                if engine.capabilities().handles_path(path) {
                    Ok(engine)
                } else {
                    Err(CoreError::Engine(EngineError::Unsupported {
                        path: path.to_string(),
                    }))
                }
            }
        }
    }

    /// Every engine, for shutdown and settings UI.
    pub fn all(&self) -> &[SharedEngine] {
        &self.engines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use setbuddy_engine::null::NullEngine;
    use std::sync::Arc;

    fn registry() -> EngineRegistry {
        // A narrow "native" engine first, a catch-all second — exactly the shape
        // v2 takes when AVFoundation joins mpv.
        EngineRegistry::new(vec![
            Arc::new(NullEngine::with_containers(
                "native",
                &["mp3", "mp4", "wav"],
            )),
            Arc::new(NullEngine::with_containers(
                "fallback",
                &["mp3", "mp4", "wav", "webm", "mkv"],
            )),
        ])
    }

    #[test]
    fn auto_prefers_the_first_engine_that_can_open_the_file() {
        let r = registry();
        assert_eq!(
            r.select_for("/a/track.mp3").unwrap().capabilities().id,
            "native"
        );
    }

    #[test]
    fn auto_falls_through_to_an_engine_that_handles_the_container() {
        let r = registry();
        assert_eq!(
            r.select_for("/sets/palms.webm").unwrap().capabilities().id,
            "fallback",
            "a webm must reach the engine that can actually open it"
        );
    }

    #[test]
    fn auto_reports_unsupported_when_nothing_can_play_it() {
        let r = registry();
        assert!(matches!(
            r.select_for("/a/notes.txt"),
            Err(CoreError::Engine(
                setbuddy_engine::EngineError::Unsupported { .. }
            ))
        ));
    }

    #[test]
    fn force_does_not_silently_fall_back() {
        let r = registry().with_policy(EnginePolicy::Force("native".into()));
        assert_eq!(
            r.select_for("/a/track.mp3").unwrap().capabilities().id,
            "native"
        );
        assert!(
            matches!(
                r.select_for("/sets/palms.webm"),
                Err(CoreError::Engine(
                    setbuddy_engine::EngineError::Unsupported { .. }
                ))
            ),
            "a forced engine that cannot open the file is an error, not a fallback"
        );
    }

    #[test]
    fn forcing_an_unknown_engine_is_an_error() {
        let r = registry().with_policy(EnginePolicy::Force("avfoundation".into()));
        assert!(matches!(
            r.select_for("/a/track.mp3"),
            Err(CoreError::Internal { .. })
        ));
    }

    #[test]
    fn policy_round_trips_through_settings() {
        assert_eq!(EnginePolicy::parse("auto"), EnginePolicy::Auto);
        assert_eq!(EnginePolicy::parse(""), EnginePolicy::Auto);
        assert_eq!(
            EnginePolicy::parse("MPV"),
            EnginePolicy::Force("mpv".into())
        );
        assert_eq!(EnginePolicy::Force("mpv".into()).as_str(), "mpv");
    }
}
