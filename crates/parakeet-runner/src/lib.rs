pub mod config;
pub mod parakeet;

pub use config::ParakeetConfig;
pub use parakeet::{ParakeetHandle, TranscribeOutput};

pub const DEFAULT_MODEL: &str = "nvidia/parakeet-tdt-0.6b-v3";
pub const CAPABILITY: &str = "asr/parakeet-tdt-0.6b-v2";
pub const DEFAULT_GUEST_ID_SUFFIX: &str = "model-controller-parakeet";
