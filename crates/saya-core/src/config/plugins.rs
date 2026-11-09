//! Explicit host bindings for enabled immutable plugin packages.
use super::invalid;
use crate::{contract::BoundaryError, plugin_runtime::PluginHostBindings};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PluginSettings {
    /// Installation and enablement still require explicit user operations.
    pub enabled: bool,
    pub bindings: PluginHostBindings,
}
impl Default for PluginSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            bindings: PluginHostBindings::default(),
        }
    }
}
impl PluginSettings {
    /// Syntax-only validation. Run assembly verifies exact package bindings,
    /// environment references, executable identities and native capabilities.
    pub fn validate(&self) -> Result<(), BoundaryError> {
        if self.bindings.runtimes.len() > 64
            || self.bindings.entries.len() > 1024
            || self.bindings.slots.len() > 4096
            || serde_json::to_vec(&self.bindings)
                .map_or(true, |bytes| bytes.len() > 4 * 1024 * 1024)
        {
            return Err(invalid("plugin host bindings exceed their limits"));
        }
        for runtime in self.bindings.runtimes.values() {
            if !runtime.program.is_absolute()
                || runtime.read_roots.len() > 64
                || runtime.read_roots.iter().any(|path| !path.is_absolute())
                || runtime.env.len() > 32
                || runtime.args_prefix.len() > 256
                || runtime
                    .args_prefix
                    .iter()
                    .any(|argument| argument.len() > 16 * 1024 || argument.contains('\0'))
            {
                return Err(invalid("plugin runtime host binding is invalid"));
            }
        }
        for entry in self.bindings.entries.values() {
            entry.limits.resolve()?;
        }
        Ok(())
    }
}
