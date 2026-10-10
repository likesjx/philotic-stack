//! Fail-closed transition guard for existing unfenced Memgraph entrypoints.
//! This is not a lease issuer. Required mode refuses these routes until their
//! canonical owner can retain a real fence through server quiescence. Defaults
//! preserve the internal legacy runtime; external production admission remains
//! separately denied until installation and all writer coverage are verified.
use anyhow::{Result, bail};

pub const EXTERNAL_READ_COORDINATION_ENV: &str = "PHILOTIC_LIFE_EXTERNAL_READ_COORDINATION";

pub fn require_unfenced_route_disabled(raw: Option<&str>) -> Result<()> {
    match raw {
        None | Some("disabled") => Ok(()),
        _ => bail!("unfenced Memgraph route unavailable under external read coordination"),
    }
}

pub fn require_memgraph_enrollment() -> Result<()> {
    match std::env::var(EXTERNAL_READ_COORDINATION_ENV) {
        Ok(value) => require_unfenced_route_disabled(Some(&value)),
        Err(std::env::VarError::NotPresent) => require_unfenced_route_disabled(None),
        Err(_) => bail!("unfenced Memgraph route unavailable under external read coordination"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_and_unknown_modes_cannot_fall_back_to_unfenced_access() {
        assert!(require_unfenced_route_disabled(None).is_ok());
        assert!(require_unfenced_route_disabled(Some("disabled")).is_ok());
        for mode in ["required", "", "enabled", "true", "disabled ", "synthetic"] {
            assert!(require_unfenced_route_disabled(Some(mode)).is_err());
        }
    }
}
