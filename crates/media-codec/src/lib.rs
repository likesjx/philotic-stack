pub mod cache;

pub use cache::CodecCache;

use anyhow::Result;
use media_prep::transcode_to_wav_16k;

/// Audio providers with known MIME-type constraints for inline attachment delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioProvider {
    Gemini,
}

impl AudioProvider {
    /// Returns true if the provider natively accepts this MIME type.
    pub fn supports_mime(self, mime: &str) -> bool {
        let m = mime.trim().to_ascii_lowercase();
        let m = m.split(';').next().unwrap_or("").trim();
        match self {
            Self::Gemini => matches!(
                m,
                "audio/wav"
                    | "audio/x-wav"
                    | "audio/mp3"
                    | "audio/mpeg"
                    | "audio/opus"
                    | "audio/webm"
                    | "audio/aac"
                    | "audio/flac"
                    | "audio/mp4"
                    | "audio/pcm"
            ),
        }
    }

    /// Target MIME type when the source is not natively supported.
    pub fn fallback_mime(self) -> &'static str {
        "audio/wav"
    }
}

pub struct NormalizeResult {
    pub bytes: Vec<u8>,
    pub mime_type: String,
    /// True if ffmpeg was invoked; false if source was already compatible or served from cache.
    pub transcoded: bool,
}

/// Normalize audio bytes for delivery to a provider.
///
/// If the provider already supports `source_mime`, bytes are returned unchanged.
/// Otherwise, the audio is transcoded to the provider's fallback format (WAV 16 kHz mono).
/// When `cache` is provided it is checked before transcoding and written to on a miss.
pub async fn normalize_audio(
    bytes: Vec<u8>,
    source_mime: &str,
    provider: AudioProvider,
    cache: Option<&CodecCache>,
    ffmpeg_bin: &str,
) -> Result<NormalizeResult> {
    if provider.supports_mime(source_mime) {
        return Ok(NormalizeResult {
            bytes,
            mime_type: source_mime.trim().to_string(),
            transcoded: false,
        });
    }

    let target_mime = provider.fallback_mime();

    if let Some(c) = cache
        && let Some(cached) = c.get(&bytes, target_mime)?
    {
        tracing::debug!(source_mime, target_mime, "media-codec: cache hit");
        return Ok(NormalizeResult {
            bytes: cached,
            mime_type: target_mime.to_string(),
            transcoded: false,
        });
    }

    let transcoded = transcode_to_wav_16k(bytes.clone(), source_mime, ffmpeg_bin).await?;

    if let Some(c) = cache
        && let Err(e) = c.put(&bytes, target_mime, &transcoded)
    {
        tracing::warn!("media-codec: cache write failed: {e:#}");
    }

    Ok(NormalizeResult {
        bytes: transcoded,
        mime_type: target_mime.to_string(),
        transcoded: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_accepts_wav() {
        assert!(AudioProvider::Gemini.supports_mime("audio/wav"));
        assert!(AudioProvider::Gemini.supports_mime("audio/x-wav"));
        assert!(AudioProvider::Gemini.supports_mime("audio/mp3"));
        assert!(AudioProvider::Gemini.supports_mime("audio/opus"));
    }

    #[test]
    fn gemini_rejects_ogg() {
        assert!(!AudioProvider::Gemini.supports_mime("audio/ogg"));
        assert!(!AudioProvider::Gemini.supports_mime("audio/x-ogg"));
        assert!(!AudioProvider::Gemini.supports_mime("audio/ogg; codecs=opus"));
    }

    #[test]
    fn gemini_fallback_is_wav() {
        assert_eq!(AudioProvider::Gemini.fallback_mime(), "audio/wav");
    }

    #[test]
    fn cache_roundtrip() {
        let cache = CodecCache::open(":memory:").unwrap();
        let src = b"fake-audio-bytes";
        let result = b"fake-wav-bytes";
        cache.put(src, "audio/wav", result).unwrap();
        let hit = cache.get(src, "audio/wav").unwrap();
        assert_eq!(hit.as_deref(), Some(result.as_ref()));
        let miss = cache.get(src, "audio/mp3").unwrap();
        assert!(miss.is_none());
    }
}
