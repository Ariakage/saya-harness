//! Strict, default-disabled settings for browser credential slots and the
//! local file broker. Every field is host-owned; model input may only name a
//! pre-registered slot alias or artifact reference.

use crate::{
    contract::{BoundaryError, ErrorInfo},
    credentials::CredentialRef,
    network::NormalizedOrigin,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_MAX_DOWNLOAD_BYTES: usize = 8 * 1024 * 1024;
pub const DEFAULT_MAX_UPLOAD_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_CREDENTIAL_SLOTS: usize = 16;
const MAX_SLOT_USES: u32 = 32;

fn error(code: &str, message: &'static str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}

fn invalid(message: &'static str) -> BoundaryError {
    error("invalid_config", message)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserCredentialPurpose {
    /// Fill one form field without submitting it.
    FormFill,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserCredentialSlot {
    /// Opaque model-visible alias.
    pub alias: String,
    /// Durable host-only reference into the private credential store.
    pub credential_ref: CredentialRef,
    /// Exact canonical origin (scheme, host and explicit port when non-default).
    pub origin: String,
    /// Only `form_fill` is supported.
    pub purpose: BrowserCredentialPurpose,
    /// Number of fill dispatches allowed for this Run.
    pub max_uses: u32,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrowserCredentialSettings {
    pub enabled: bool,
    /// Private credential store directory. No value is ever read from model
    /// input.
    pub store: Option<PathBuf>,
    pub slots: Vec<BrowserCredentialSlot>,
}

/// Canonical exact-origin rendering used by slot validation and matching.
pub fn exact_origin(value: &str) -> Result<(NormalizedOrigin, String), BoundaryError> {
    let origin =
        NormalizedOrigin::parse(value).map_err(|_| invalid("Browser origin is invalid"))?;
    let scheme = match origin.scheme {
        crate::network::OriginScheme::Http => "http",
        crate::network::OriginScheme::Https => "https",
    };
    let canonical = format!("{scheme}://{}", origin.authority());
    if canonical != value {
        return Err(invalid(
            "Browser origin must be an exact canonical origin without a path",
        ));
    }
    Ok((origin, canonical))
}

/// Credential slots may target HTTPS only. A literal loopback HTTP origin is
/// allowed solely as the explicit local-development exception.
fn credential_origin_allowed(origin: &NormalizedOrigin) -> bool {
    match origin.scheme {
        crate::network::OriginScheme::Https => true,
        crate::network::OriginScheme::Http => {
            origin.host == "localhost"
                || origin
                    .host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        }
    }
}

impl BrowserCredentialSettings {
    pub fn validate(&self) -> Result<(), BoundaryError> {
        if !self.enabled {
            if self.store.is_some() || !self.slots.is_empty() {
                return Err(invalid(
                    "Disabled browser credentials must not declare a store or slots",
                ));
            }
            return Ok(());
        }
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| invalid("Enabled browser credentials require a store path"))?;
        if !store.is_absolute()
            || store.as_os_str().is_empty()
            || store.to_str().is_none()
            || store.as_os_str().to_string_lossy().contains('\0')
        {
            return Err(invalid(
                "Browser credential store must be an absolute UTF-8 path",
            ));
        }
        if self.slots.is_empty() || self.slots.len() > MAX_CREDENTIAL_SLOTS {
            return Err(invalid(
                "Browser credential slots are empty or exceed their limit",
            ));
        }
        let mut aliases = std::collections::BTreeSet::new();
        for slot in &self.slots {
            if !valid_alias(&slot.alias) || !aliases.insert(slot.alias.as_str()) {
                return Err(invalid(
                    "Browser credential slot alias is invalid or duplicated",
                ));
            }
            if slot.max_uses == 0 || slot.max_uses > MAX_SLOT_USES {
                return Err(invalid("Browser credential slot max_uses is invalid"));
            }
            let (origin, _) = exact_origin(&slot.origin)?;
            if !credential_origin_allowed(&origin) {
                return Err(invalid(
                    "Browser credential slot origin must be HTTPS except an explicit loopback development origin",
                ));
            }
        }
        Ok(())
    }
}

pub fn valid_alias(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrowserFileSettings {
    pub enabled: bool,
    pub max_download_bytes: usize,
    pub max_upload_bytes: usize,
}

impl Default for BrowserFileSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_download_bytes: DEFAULT_MAX_DOWNLOAD_BYTES,
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
        }
    }
}

impl BrowserFileSettings {
    pub fn validate(&self) -> Result<(), BoundaryError> {
        if self.max_download_bytes == 0
            || self.max_download_bytes > crate::media::MAX_FILE_BYTES as usize
            || self.max_upload_bytes == 0
            || self.max_upload_bytes > crate::media::MAX_FILE_BYTES as usize
        {
            return Err(invalid("Browser file broker limits are invalid"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn settings(origin: &str) -> BrowserCredentialSettings {
        BrowserCredentialSettings {
            enabled: true,
            store: Some(std::env::temp_dir().join("soyo-browser-credentials")),
            slots: vec![BrowserCredentialSlot {
                alias: "login".into(),
                credential_ref: CredentialRef::from_str("018f4f7e-7a3c-7c4e-9f5a-123456789abc")
                    .unwrap(),
                origin: origin.into(),
                purpose: BrowserCredentialPurpose::FormFill,
                max_uses: 1,
            }],
        }
    }

    #[test]
    fn credential_slots_require_https_except_loopback_development() {
        assert!(settings("https://login.example").validate().is_ok());
        assert!(settings("http://localhost:8080").validate().is_ok());
        assert!(settings("http://127.0.0.1:8080").validate().is_ok());
        assert!(settings("http://[::1]:8080").validate().is_ok());
        let error = settings("http://login.example").validate().unwrap_err();
        assert_eq!(error.0.code, "invalid_config");
        let error = settings("https://login.example/path")
            .validate()
            .unwrap_err();
        assert_eq!(error.0.code, "invalid_config");
    }
}
