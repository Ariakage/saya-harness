//! Origin-bound credential slots for the browser fill action.
//!
//! Model input names a slot alias only. The host resolves the opaque
//! `CredentialRef` from a private store, checks the exact current page origin,
//! the slot purpose, revocation and the remaining use count, and exposes the
//! plaintext only at the final provider call boundary. The value is registered
//! with the shared `SecretRegistry` before dispatch so intent, results, logs,
//! OutputStore projections and compacted context never carry it.

use super::{
    BrowserFailure,
    settings::{BrowserCredentialPurpose, BrowserCredentialSettings},
};
use crate::{
    credentials::{
        CredentialRef, CredentialScope, FileCredentialStore, SecretRegistry, SecretValue,
    },
    network::NormalizedOrigin,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Debug, Default)]
struct SlotUse {
    uses: u32,
}

pub(crate) struct BrowserCredentialBroker {
    store: FileCredentialStore,
    slots: BTreeMap<String, BrowserCredentialSlotSettings>,
    uses: Mutex<BTreeMap<String, SlotUse>>,
}

#[derive(Debug, Clone)]
struct BrowserCredentialSlotSettings {
    reference: CredentialRef,
    origin: NormalizedOrigin,
    max_uses: u32,
    purpose: BrowserCredentialPurpose,
}

/// A resolved fill value. It carries no model-visible plaintext field.
pub(crate) struct ResolvedCredentialSlot {
    pub(crate) alias: String,
    value: SecretValue,
    pub(crate) remaining_uses: u32,
}

impl std::fmt::Debug for ResolvedCredentialSlot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedCredentialSlot")
            .field("alias", &self.alias)
            .field("remaining_uses", &self.remaining_uses)
            .finish_non_exhaustive()
    }
}

impl ResolvedCredentialSlot {
    /// Expose the value only at the final provider dispatch boundary.
    pub(crate) fn expose(&self) -> &str {
        self.value.expose()
    }
}

fn failure(code: &str, message: impl Into<String>) -> BrowserFailure {
    BrowserFailure::new(code, message)
}

impl BrowserCredentialBroker {
    pub(crate) fn from_settings(
        settings: &BrowserCredentialSettings,
        registry: Arc<SecretRegistry>,
    ) -> Result<Self, super::BoundaryError> {
        settings.validate()?;
        let store_path = settings
            .store
            .as_ref()
            .ok_or_else(|| super::invalid_config("Browser credential store path is missing"))?;
        let store = FileCredentialStore::open(store_path, registry)?;
        Self::new(settings, store)
    }

    pub(crate) fn new(
        settings: &BrowserCredentialSettings,
        store: FileCredentialStore,
    ) -> Result<Self, super::BoundaryError> {
        settings.validate()?;
        let mut slots = BTreeMap::new();
        for slot in &settings.slots {
            let origin = NormalizedOrigin::parse(&slot.origin)
                .map_err(|_| super::invalid_config("Browser credential origin is invalid"))?;
            slots.insert(
                slot.alias.clone(),
                BrowserCredentialSlotSettings {
                    reference: slot.credential_ref,
                    origin,
                    max_uses: slot.max_uses,
                    purpose: slot.purpose,
                },
            );
        }
        Ok(Self {
            store,
            slots,
            uses: Mutex::new(BTreeMap::new()),
        })
    }

    /// Check the slot against the exact current document origin and its
    /// remaining uses. Revocation is checked by the store during `resolve`.
    fn check(
        &self,
        alias: &str,
        origin: &NormalizedOrigin,
    ) -> Result<(CredentialRef, u32), BrowserFailure> {
        let slot = self.slots.get(alias).ok_or_else(|| {
            failure(
                "credential_slot_unknown",
                "Browser credential slot is unknown",
            )
        })?;
        if slot.purpose != BrowserCredentialPurpose::FormFill {
            return Err(failure(
                "credential_purpose_denied",
                "Browser credential slot purpose is not form_fill",
            ));
        }
        if &slot.origin != origin {
            return Err(failure(
                "credential_origin_mismatch",
                "Browser credential slot is bound to a different origin",
            ));
        }
        let uses = self
            .uses
            .lock()
            .map_err(|_| failure("credential_state", "Browser credential use lock failed"))?
            .get(alias)
            .map_or(0, |used| used.uses);
        if uses >= slot.max_uses {
            return Err(failure(
                "credential_uses_exhausted",
                "Browser credential slot has reached its use limit",
            ));
        }
        Ok((slot.reference, slot.max_uses - uses))
    }

    /// Resolve one slot for a fill dispatch. The returned value must be
    /// consumed exactly once at the provider boundary.
    pub(crate) async fn resolve(
        &self,
        alias: &str,
        origin: &NormalizedOrigin,
    ) -> Result<ResolvedCredentialSlot, BrowserFailure> {
        let (reference, remaining) = self.check(alias, origin)?;
        let scope = CredentialScope::for_browser(alias, origin)
            .map_err(|_| failure("credential_scope", "Browser credential scope is invalid"))?;
        let resolved = self
            .store
            .resolve(reference, &scope)
            .await
            .map_err(|error| match error.0.code.as_str() {
                "credential_revoked" => failure(
                    "credential_revoked",
                    "Browser credential slot has been revoked",
                ),
                "credential_not_found" | "credential_corrupt" => failure(
                    "credential_unavailable",
                    "Browser credential slot cannot be resolved",
                ),
                _ => failure(
                    "credential_unavailable",
                    "Browser credential slot cannot be resolved",
                ),
            })?;
        let value = match resolved.value {
            crate::credentials::CredentialValue::Secret(value) => value,
            // OAuth material is not a form-fill text value.
            crate::credentials::CredentialValue::OAuth(_) => {
                return Err(failure(
                    "credential_purpose_denied",
                    "Browser form_fill slots require a secret value",
                ));
            }
        };
        Ok(ResolvedCredentialSlot {
            alias: alias.into(),
            value,
            remaining_uses: remaining,
        })
    }

    /// Consume one use immediately before dispatch. An unknown outcome keeps
    /// the use consumed because the value may have reached the page.
    pub(crate) fn consume(&self, alias: &str) -> Result<u32, BrowserFailure> {
        let mut uses = self
            .uses
            .lock()
            .map_err(|_| failure("credential_state", "Browser credential use lock failed"))?;
        let slot = self.slots.get(alias).ok_or_else(|| {
            failure(
                "credential_slot_unknown",
                "Browser credential slot is unknown",
            )
        })?;
        let entry = uses.entry(alias.into()).or_insert(SlotUse { uses: 0 });
        if entry.uses >= slot.max_uses {
            return Err(failure(
                "credential_uses_exhausted",
                "Browser credential slot has reached its use limit",
            ));
        }
        entry.uses += 1;
        Ok(slot.max_uses - entry.uses)
    }
}
