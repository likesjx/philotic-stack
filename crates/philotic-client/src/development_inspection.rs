//! Fixed snapshot inspection protocol; paths are labels, never filesystem authority.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
pub const ACTION: &str = "development.inspect_manifest";
pub const KEY: &str = "__development_inspection__";
pub const CHECKPOINT: &str = "development-inspection-checkpoint";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub cargo: Vec<u8>,
    pub readme: Vec<u8>,
}
impl Snapshot {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.cargo.len() + self.readme.len() <= 16384,
            "snapshot too large"
        );
        Ok(())
    }
    pub fn digest(&self) -> Result<String> {
        self.validate()?;
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)))
    }
    pub fn inspect(&self) -> Result<serde_json::Value> {
        self.validate()?;
        Ok(serde_json::json!({"input_digest":self.digest()?,"files":[
            {"path":"Cargo.toml","bytes":self.cargo.len(),"sha256":format!("{:x}",Sha256::digest(&self.cargo))},
            {"path":"README.md","bytes":self.readme.len(),"sha256":format!("{:x}",Sha256::digest(&self.readme))}
        ]}))
    }
}
