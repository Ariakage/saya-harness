//! Content and dependency identities, independent of installation or enablement.
use super::{
    Dependency, Manifest, PluginError, Result, VerifiedPackage, digest_hex, https_origin, invalid,
    valid_digest,
};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const MAX_LOCK_BYTES: usize = 256 * 1024;
const MAX_LOCK_PACKAGES: usize = 128;
const MAX_ALIASES: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    LocalDirectory,
    Archive,
    Https,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackageSource {
    pub kind: SourceKind,
    pub reference: String,
}
impl PackageSource {
    pub fn validate(&self) -> Result<()> {
        if self.reference.trim().is_empty()
            || self.reference.len() > 2048
            || self.reference.chars().any(crate::text::is_terminal_control)
        {
            return Err(invalid("package source reference is unsafe or too large"));
        }
        if self.kind == SourceKind::Https {
            https_origin(&self.reference, false)?;
        } else if self.reference.contains("://") {
            return Err(invalid(
                "local package source cannot contain a retrieval URI",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DependencyPin {
    pub id: String,
    pub version: String,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LockedPackage {
    pub id: String,
    pub version: String,
    pub source: PackageSource,
    pub digest: String,
    pub capability_fingerprint: String,
    #[serde(default)]
    pub dependencies: Vec<DependencyPin>,
}

/// A model-facing name bound to the complete executable identity. User aliases
/// still require an explicit scope/enablement record in the package manager.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ToolAlias {
    pub name: String,
    pub plugin_id: String,
    pub entry_id: String,
    pub package_digest: String,
    pub tool_version: String,
}
impl ToolAlias {
    pub fn stable(
        plugin_id: &str,
        entry_id: &str,
        package_digest: &str,
        tool_version: &str,
    ) -> Result<Self> {
        let mut value = Self {
            name: String::new(),
            plugin_id: plugin_id.into(),
            entry_id: entry_id.into(),
            package_digest: package_digest.into(),
            tool_version: tool_version.into(),
        };
        value.validate_identity()?;
        let identity = format!("{plugin_id}\0{entry_id}\0{package_digest}\0{tool_version}");
        let hash = digest_hex(identity.as_bytes());
        let readable = &entry_id[..entry_id.len().min(18)];
        value.name = format!("plugin__{readable}_{}", &hash[..20]);
        Ok(value)
    }
    fn validate_identity(&self) -> Result<()> {
        if !valid_plugin_id(&self.plugin_id)
            || !valid_entry_id(&self.entry_id)
            || !valid_digest(&self.package_digest)
            || self.tool_version.trim().is_empty()
            || self.tool_version.len() > 256
            || self
                .tool_version
                .chars()
                .any(crate::text::is_terminal_control)
        {
            return Err(invalid("tool alias identity is invalid"));
        }
        Ok(())
    }
    fn validate_name(&self) -> Result<()> {
        self.validate_identity()?;
        if self.name.len() > 64
            || !self
                .name
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
            || !self
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
            || self.name.starts_with("mcp__")
            || crate::registry::builtin_effect(&self.name).is_some()
        {
            return Err(invalid("tool alias name is invalid or reserved"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginLock {
    pub schema_version: u32,
    pub digest_algorithm: String,
    pub packages: Vec<LockedPackage>,
    #[serde(default)]
    pub aliases: Vec<ToolAlias>,
}
impl PluginLock {
    /// Resolve solely from packages explicitly supplied by the caller. No source
    /// is fetched and no dependency, capability or enablement is inferred.
    pub fn build(
        packages: &[(&VerifiedPackage, PackageSource)],
        aliases: Vec<ToolAlias>,
    ) -> Result<Self> {
        if packages.is_empty() || packages.len() > MAX_LOCK_PACKAGES {
            return Err(invalid("lock package count is invalid"));
        }
        let mut supplied: BTreeMap<&str, (&VerifiedPackage, &PackageSource)> = BTreeMap::new();
        for (package, source) in packages {
            source.validate()?;
            let metadata = &package.manifest().plugin;
            if let Some((previous, _)) = supplied.get(metadata.id.as_str()) {
                if previous.manifest().plugin.version == metadata.version
                    && previous.digest() != package.digest()
                {
                    return Err(PluginError {
                        code: "plugin_source_conflict",
                        message: "same plugin version has conflicting content",
                    });
                }
                return Err(invalid("lock contains multiple selections for a plugin ID"));
            }
            supplied.insert(metadata.id.as_str(), (*package, source));
        }
        let mut locked = Vec::with_capacity(packages.len());
        for (package, source) in supplied.values() {
            let manifest = package.manifest();
            let mut dependencies = Vec::new();
            for dependency in &manifest.dependencies {
                let (resolved, _) = supplied
                    .get(dependency.id.as_str())
                    .ok_or_else(|| invalid("plugin dependency is unavailable"))?;
                matches_requirement(dependency, &resolved.manifest().plugin.version)?;
                dependencies.push(DependencyPin {
                    id: dependency.id.clone(),
                    version: resolved.manifest().plugin.version.clone(),
                    digest: resolved.digest().into(),
                });
            }
            dependencies.sort_by(|left, right| left.id.cmp(&right.id));
            locked.push(LockedPackage {
                id: manifest.plugin.id.clone(),
                version: manifest.plugin.version.clone(),
                source: (**source).clone(),
                digest: package.digest().into(),
                capability_fingerprint: package.capability_fingerprint().into(),
                dependencies,
            });
        }
        let mut value = Self {
            schema_version: 1,
            digest_algorithm: super::package::TREE_DIGEST_ALGORITHM.into(),
            packages: locked,
            aliases,
        };
        value
            .aliases
            .sort_by(|left, right| left.name.cmp(&right.name));
        value.validate()?;
        for (package, _) in packages {
            value.verify_package(package)?;
        }
        Ok(value)
    }

    /// Parse a bounded TOML lock, rejecting unknown fields and invalid graphs.
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() > MAX_LOCK_BYTES {
            return Err(invalid("plugin lock exceeds byte limit"));
        }
        let lock: Self =
            toml::from_str(text).map_err(|_| invalid("plugin lock TOML is invalid"))?;
        lock.validate()?;
        Ok(lock)
    }
    pub fn to_toml(&self) -> Result<String> {
        self.validate()?;
        let text = toml::to_string(self).map_err(|_| invalid("cannot encode plugin lock"))?;
        if text.len() > MAX_LOCK_BYTES {
            return Err(invalid("plugin lock exceeds byte limit"));
        }
        Ok(text)
    }

    pub fn validate(&self) -> Result<()> {
        let bytes = serde_json::to_vec(self).map_err(|_| invalid("cannot encode plugin lock"))?;
        if bytes.len() > MAX_LOCK_BYTES
            || self.schema_version != 1
            || self.digest_algorithm != super::package::TREE_DIGEST_ALGORITHM
            || self.packages.is_empty()
            || self.packages.len() > MAX_LOCK_PACKAGES
            || self.aliases.len() > MAX_ALIASES
        {
            return Err(invalid("plugin lock version or limits are invalid"));
        }
        let mut selected = BTreeMap::new();
        for package in &self.packages {
            if !valid_plugin_id(&package.id)
                || !canonical_version(&package.version)
                || !valid_digest(&package.digest)
                || !valid_digest(&package.capability_fingerprint)
                || package.dependencies.len() > 32
                || selected.insert(package.id.as_str(), package).is_some()
            {
                return Err(invalid("locked package identity is invalid or duplicated"));
            }
            package.source.validate()?;
        }
        for package in &self.packages {
            let mut dependencies = BTreeSet::new();
            for pin in &package.dependencies {
                let resolved = selected
                    .get(pin.id.as_str())
                    .ok_or_else(|| invalid("locked dependency is missing"))?;
                if !dependencies.insert(pin.id.as_str())
                    || pin.version != resolved.version
                    || pin.digest != resolved.digest
                {
                    return Err(invalid(
                        "locked dependency pin does not match selected content",
                    ));
                }
            }
        }
        let mut visiting = BTreeSet::new();
        let mut complete = BTreeSet::new();
        for id in selected.keys() {
            visit(id, &selected, &mut visiting, &mut complete)?;
        }
        let mut aliases = BTreeSet::new();
        for alias in &self.aliases {
            alias.validate_name()?;
            let package = selected
                .get(alias.plugin_id.as_str())
                .ok_or_else(|| invalid("tool alias package is missing"))?;
            if alias.package_digest != package.digest || !aliases.insert(alias.name.as_str()) {
                return Err(invalid(
                    "tool alias collides or refers to different package content",
                ));
            }
        }
        Ok(())
    }

    /// Compare against the sealed bytes about to be installed or activated.
    /// The caller must still verify host compatibility and current permissions.
    pub fn verify_package(&self, package: &VerifiedPackage) -> Result<()> {
        self.validate()?;
        let manifest = package.manifest();
        let locked = self
            .packages
            .iter()
            .find(|locked| locked.id == manifest.plugin.id)
            .ok_or_else(|| invalid("package is absent from plugin lock"))?;
        if locked.version != manifest.plugin.version
            || locked.digest != package.digest()
            || locked.capability_fingerprint != package.capability_fingerprint()
            || locked.dependencies.len() != manifest.dependencies.len()
        {
            return Err(invalid("sealed package does not match its lock identity"));
        }
        for dependency in &manifest.dependencies {
            let pin = locked
                .dependencies
                .iter()
                .find(|pin| pin.id == dependency.id)
                .ok_or_else(|| invalid("manifest dependency is absent from plugin lock"))?;
            matches_requirement(dependency, &pin.version)?;
        }
        for alias in self
            .aliases
            .iter()
            .filter(|alias| alias.plugin_id == manifest.plugin.id)
        {
            if !declares_entry(manifest, &alias.entry_id) {
                return Err(invalid("tool alias entry is absent from its package"));
            }
        }
        Ok(())
    }
}

fn visit<'a>(
    id: &'a str,
    selected: &BTreeMap<&'a str, &'a LockedPackage>,
    visiting: &mut BTreeSet<&'a str>,
    complete: &mut BTreeSet<&'a str>,
) -> Result<()> {
    if complete.contains(id) {
        return Ok(());
    }
    if !visiting.insert(id) {
        return Err(invalid("plugin dependency graph contains a cycle"));
    }
    let package = *selected
        .get(id)
        .ok_or_else(|| invalid("locked dependency is missing"))?;
    for pin in &package.dependencies {
        visit(&pin.id, selected, visiting, complete)?;
    }
    visiting.remove(id);
    complete.insert(id);
    Ok(())
}
fn matches_requirement(dependency: &Dependency, version: &str) -> Result<()> {
    let requirement = VersionReq::parse(&dependency.version)
        .map_err(|_| invalid("dependency version range is invalid"))?;
    let version = Version::parse(version).map_err(|_| invalid("dependency version is invalid"))?;
    if !requirement.matches(&version) {
        return Err(invalid(
            "dependency version does not satisfy its requirement",
        ));
    }
    Ok(())
}
fn declares_entry(manifest: &Manifest, id: &str) -> bool {
    manifest.skills.iter().any(|entry| entry.id == id)
        || manifest.resources.iter().any(|entry| entry.id == id)
        || manifest.mcp.iter().any(|entry| entry.id() == id)
        || manifest.hooks.iter().any(|entry| entry.id == id)
}
fn canonical_version(version: &str) -> bool {
    version.len() <= 128
        && Version::parse(version).is_ok_and(|parsed| parsed.to_string() == version)
}
fn valid_plugin_id(id: &str) -> bool {
    (3..=128).contains(&id.len())
        && id.contains('.')
        && id.split('.').all(|segment| {
            segment.split('-').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            })
        })
}
fn valid_entry_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.as_bytes()[0].is_ascii_lowercase()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}
