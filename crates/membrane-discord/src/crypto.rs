use aes_gcm::{
    Aes256Gcm, Key, Nonce as AesNonce,
    aead::{Aead, KeyInit, Payload},
};
/// Discord voice transport encryption.
///
/// Discord supports two encryption modes (preference order):
///   1. aead_aes256_gcm_rtpsize   (preferred)
///   2. aead_xchacha20_poly1305_rtpsize (required fallback)
///
/// In rtpsize variants the nonce is appended to the ciphertext rather than
/// prepended, and the AEAD tag is appended after.  The 4-byte RTP extension
/// header is used as nonce material when present; otherwise a 4-byte counter
/// is appended as the nonce.
///
/// Reference: https://discord.com/developers/docs/topics/voice-connections
use anyhow::{Result, bail};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead as _, KeyInit as _},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncryptionMode {
    AeadAes256GcmRtpSize,
    AeadXChaCha20Poly1305RtpSize,
}

impl EncryptionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AeadAes256GcmRtpSize => "aead_aes256_gcm_rtpsize",
            Self::AeadXChaCha20Poly1305RtpSize => "aead_xchacha20_poly1305_rtpsize",
        }
    }

    /// Parse from the string Discord returns in Session Description.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "aead_aes256_gcm_rtpsize" => Some(Self::AeadAes256GcmRtpSize),
            "aead_xchacha20_poly1305_rtpsize" => Some(Self::AeadXChaCha20Poly1305RtpSize),
            _ => None,
        }
    }

    /// Preferred mode list to send in Select Protocol, in order.
    pub fn preference_list() -> &'static [&'static str] {
        &["aead_aes256_gcm_rtpsize", "aead_xchacha20_poly1305_rtpsize"]
    }
}

/// Active voice session encryption state, obtained from Opcode 4 Session Description.
pub struct VoiceEncryptionState {
    mode: EncryptionMode,
    secret_key: Vec<u8>,
    /// Monotonic nonce counter for outbound packets.
    nonce_counter: u32,
}

impl VoiceEncryptionState {
    pub fn new(mode: EncryptionMode, secret_key: Vec<u8>) -> Self {
        Self {
            mode,
            secret_key,
            nonce_counter: 0,
        }
    }

    pub fn secret_key(&self) -> &[u8] {
        &self.secret_key
    }

    /// Encrypt an Opus payload for outbound RTP.
    ///
    /// Returns the encrypted payload bytes to be placed after the RTP header.
    /// The 4-byte nonce counter is appended at the end (rtpsize convention).
    pub fn encrypt(&mut self, rtp_header: &[u8], opus_payload: &[u8]) -> Result<Vec<u8>> {
        let nonce_val = self.nonce_counter;
        self.nonce_counter = self.nonce_counter.wrapping_add(1);

        let nonce_bytes = nonce_val.to_be_bytes();

        match self.mode {
            EncryptionMode::AeadAes256GcmRtpSize => {
                // 12-byte nonce: 8 zero bytes + 4-byte counter
                let mut nonce = [0u8; 12];
                nonce[8..].copy_from_slice(&nonce_bytes);

                let key = Key::<Aes256Gcm>::from_slice(&self.secret_key);
                let cipher = Aes256Gcm::new(key);
                let aes_nonce = AesNonce::from_slice(&nonce);

                let ciphertext = cipher
                    .encrypt(
                        aes_nonce,
                        Payload {
                            msg: opus_payload,
                            aad: rtp_header,
                        },
                    )
                    .map_err(|e| anyhow::anyhow!("AES-GCM encrypt failed: {:?}", e))?;

                // rtpsize: append 4-byte nonce after ciphertext+tag
                let mut out = ciphertext;
                out.extend_from_slice(&nonce_bytes);
                Ok(out)
            }

            EncryptionMode::AeadXChaCha20Poly1305RtpSize => {
                // 24-byte nonce: 20 zero bytes + 4-byte counter
                let mut nonce = [0u8; 24];
                nonce[20..].copy_from_slice(&nonce_bytes);

                let key = chacha20poly1305::Key::from_slice(&self.secret_key);
                let cipher = XChaCha20Poly1305::new(key);
                let xchacha_nonce = XNonce::from_slice(&nonce);

                let ciphertext = cipher
                    .encrypt(
                        xchacha_nonce,
                        Payload {
                            msg: opus_payload,
                            aad: rtp_header,
                        },
                    )
                    .map_err(|e| anyhow::anyhow!("XChaCha20 encrypt failed: {:?}", e))?;

                let mut out = ciphertext;
                out.extend_from_slice(&nonce_bytes);
                Ok(out)
            }
        }
    }

    /// Decrypt an inbound RTP payload from Discord's SFU.
    ///
    /// `rtp_header` is the raw 12-byte (or extended) RTP header (used as AAD).
    /// `encrypted_payload` is everything after the RTP header (ciphertext + tag + 4-byte nonce).
    pub fn decrypt(&self, rtp_header: &[u8], encrypted_payload: &[u8]) -> Result<Vec<u8>> {
        if encrypted_payload.len() < 4 {
            bail!("Encrypted payload too short to contain nonce");
        }

        // rtpsize: nonce is the last 4 bytes
        let nonce_bytes: [u8; 4] = encrypted_payload[encrypted_payload.len() - 4..]
            .try_into()
            .unwrap();
        let ciphertext = &encrypted_payload[..encrypted_payload.len() - 4];

        match self.mode {
            EncryptionMode::AeadAes256GcmRtpSize => {
                let mut nonce = [0u8; 12];
                nonce[8..].copy_from_slice(&nonce_bytes);

                let key = Key::<Aes256Gcm>::from_slice(&self.secret_key);
                let cipher = Aes256Gcm::new(key);
                let aes_nonce = AesNonce::from_slice(&nonce);

                cipher
                    .decrypt(
                        aes_nonce,
                        Payload {
                            msg: ciphertext,
                            aad: rtp_header,
                        },
                    )
                    .map_err(|e| anyhow::anyhow!("AES-GCM decrypt failed: {:?}", e))
            }

            EncryptionMode::AeadXChaCha20Poly1305RtpSize => {
                let mut nonce = [0u8; 24];
                nonce[20..].copy_from_slice(&nonce_bytes);

                let key = chacha20poly1305::Key::from_slice(&self.secret_key);
                let cipher = XChaCha20Poly1305::new(key);
                let xchacha_nonce = XNonce::from_slice(&nonce);

                cipher
                    .decrypt(
                        xchacha_nonce,
                        Payload {
                            msg: ciphertext,
                            aad: rtp_header,
                        },
                    )
                    .map_err(|e| anyhow::anyhow!("XChaCha20 decrypt failed: {:?}", e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_key() -> Vec<u8> {
        (0u8..32).collect()
    }

    #[test]
    fn aes_gcm_roundtrip() {
        let key = make_key();
        let mut enc = VoiceEncryptionState::new(EncryptionMode::AeadAes256GcmRtpSize, key.clone());
        let dec = VoiceEncryptionState::new(EncryptionMode::AeadAes256GcmRtpSize, key);

        let header = b"\x80\x78\x00\x01\x00\x00\x00\x01\x00\x00\x01\x00";
        let payload = b"Hello Opus frame";

        let ciphertext = enc.encrypt(header, payload).unwrap();
        let plaintext = dec.decrypt(header, &ciphertext).unwrap();

        assert_eq!(plaintext, payload);
    }

    #[test]
    fn xchacha_roundtrip() {
        let key = make_key();
        let mut enc =
            VoiceEncryptionState::new(EncryptionMode::AeadXChaCha20Poly1305RtpSize, key.clone());
        let dec = VoiceEncryptionState::new(EncryptionMode::AeadXChaCha20Poly1305RtpSize, key);

        let header = b"\x80\x78\x00\x01\x00\x00\x00\x01\x00\x00\x01\x00";
        let payload = b"Hello Opus frame";

        let ciphertext = enc.encrypt(header, payload).unwrap();
        let plaintext = dec.decrypt(header, &ciphertext).unwrap();

        assert_eq!(plaintext, payload);
    }
}
