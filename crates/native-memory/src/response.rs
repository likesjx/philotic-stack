//! Authenticated result binding. This provides integrity, NOT confidentiality.
use super::*;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseVerifier {
    pub destination: String,
    pub runner: String,
    pub key_id: String,
    pub public_key: [u8; 32],
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseSigner {
    key_id: String,
    signing_seed: [u8; 32],
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedResponse {
    pub key_id: String,
    pub request_binding: [u8; 32],
    pub issued_at: u64,
    pub expires_at: u64,
    pub result: Response,
    pub signature: Vec<u8>,
}
impl std::fmt::Debug for SignedResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SignedResponse([REDACTED])")
    }
}
fn signing_bytes(r: &SignedResponse) -> Result<Vec<u8>> {
    let mut value = b"philotic/native-memory/response/v1\0".to_vec();
    value.extend(bytes(&(
        &r.key_id,
        r.request_binding,
        r.issued_at,
        r.expires_at,
        &r.result,
    ))?);
    Ok(value)
}
impl ResponseSigner {
    pub fn load(path: &Path) -> Result<Self> {
        load_private_json(path)
    }
    pub fn new(key_id: String, signing_seed: [u8; 32]) -> Self {
        Self {
            key_id,
            signing_seed,
        }
    }
    /// Call only after verified execution/commit. Never sign an unverified error route.
    pub fn sign(&self, envelope: &Envelope, result: Response, now: u64) -> Result<SignedResponse> {
        if now < envelope.claims.issued_at || now >= envelope.claims.expires_at {
            return Err(Error::Expired);
        }
        let mut response = SignedResponse {
            key_id: self.key_id.clone(),
            request_binding: digest(envelope)?,
            issued_at: now,
            expires_at: envelope.claims.expires_at,
            result,
            signature: Vec::new(),
        };
        response.signature = SigningKey::from_bytes(&self.signing_seed)
            .sign(&signing_bytes(&response)?)
            .to_bytes()
            .to_vec();
        Ok(response)
    }
}
pub struct ResponseExpectation {
    envelope: Envelope,
}
impl ResponseExpectation {
    pub fn new(envelope: Envelope, verifiers: &[ResponseVerifier]) -> Result<Self> {
        let this = Self { envelope };
        this.verifier(verifiers)?;
        Ok(this)
    }
    fn verifier<'a>(&self, verifiers: &'a [ResponseVerifier]) -> Result<&'a ResponseVerifier> {
        let c = &self.envelope.claims;
        let mut matched = verifiers
            .iter()
            .filter(|v| v.destination == c.destination && v.runner == c.runner);
        let verifier = matched.next().ok_or(Error::Denied)?;
        if matched.next().is_some() {
            return Err(Error::Denied);
        }
        Ok(verifier)
    }
    /// Verifier pins are reloaded by ingress for every reply; removal fails closed.
    pub fn verify(
        &self,
        response: &SignedResponse,
        verifiers: &[ResponseVerifier],
        now: u64,
    ) -> Result<Response> {
        let c = &self.envelope.claims;
        let request = &self.envelope.request;
        let verifier = self.verifier(verifiers)?;
        if response.key_id != verifier.key_id || response.request_binding != digest(&self.envelope)?
        {
            return Err(Error::Denied);
        }
        let key = VerifyingKey::from_bytes(&verifier.public_key).map_err(|_| Error::Denied)?;
        key.verify_strict(
            &signing_bytes(response)?,
            &Signature::from_slice(&response.signature).map_err(|_| Error::Denied)?,
        )
        .map_err(|_| Error::Denied)?;
        if response.issued_at < c.issued_at
            || response.issued_at > now
            || response.expires_at != c.expires_at
            || now >= response.expires_at
        {
            return Err(Error::Expired);
        }
        let result = &response.result;
        if result.receipt.event != request.event
            || result.receipt.operation != request.command.operation()
            || result.receipt.status != "committed"
            || result.receipt.committed_at > now
        {
            return Err(Error::Denied);
        }
        let limit = match request.command {
            Command::Recall { limit, .. } => limit as usize,
            Command::Remember { .. } => 0,
        };
        if result.observations.len() > limit
            || result
                .observations
                .iter()
                .any(|o| o.owner != request.owner || o.kind != "observation")
        {
            return Err(Error::Denied);
        }
        Ok(result.clone())
    }
}
