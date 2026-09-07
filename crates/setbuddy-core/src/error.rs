use setwave_engine::EngineError;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("library storage failed: {message}")]
    Storage { message: String },

    #[error(transparent)]
    Engine(#[from] EngineError),

    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("nothing in the library matches \"{query}\"")]
    NoMatch { query: String },

    #[error("{path} is not a media file Setwave recognises")]
    NotMedia { path: String },

    #[error("nothing is playing")]
    NothingPlaying,

    #[error("{message}")]
    Internal { message: String },
}

pub type Result<T> = std::result::Result<T, CoreError>;
