use super::*;
use std::{ffi::OsString, fs, os::unix::fs::PermissionsExt};

pub(super) fn validate_host(
    context: &PluginRunContext,
    bindings: &PluginHostBindings,
    pool: &ViewPool,
) -> Result<(), BoundaryError> {
    if bindings.runtimes.len() > 64
        || bindings.slots.len() > 4096
        || bindings.entries.len() > 1024
        || context.protected_roots.len() > 256
        || context.host_write_roots.len() > 256
    {
        return Err(error(
            "plugin_limit",
            "Plugin host binding count exceeds its limit",
        ));
    }
    let workspace = fs::canonicalize(&context.workspace)
        .map_err(|_| error("plugin_scope", "Plugin workspace is unavailable"))?;
    if !workspace.is_dir() {
        return Err(error(
            "plugin_scope",
            "Plugin workspace must be a directory",
        ));
    }
    pool.validate_location(
        &workspace,
        &context.protected_roots,
        &context.host_write_roots,
    )?;
    if semver::Version::parse(&context.host_version).is_err() {
        return Err(error("plugin_host", "Plugin host version is invalid"));
    }
    let metadata = serde_json::to_string(bindings)
        .map_err(|_| error("plugin_host", "Plugin host bindings cannot be encoded"))?;
    if metadata.len() > 4 * 1024 * 1024
        || crate::redaction::contains_secret(&metadata, &context.secrets.snapshot()?)
    {
        return Err(error(
            "sensitive_data",
            "Plugin host binding metadata contains protected data or exceeds its limit",
        ));
    }
    for (name, runtime) in &bindings.runtimes {
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            || !runtime.program.is_absolute()
            || runtime.args_prefix.len() > 128
            || runtime.args_prefix.iter().map(String::len).sum::<usize>() > 32 * 1024
            || runtime
                .args_prefix
                .iter()
                .any(|arg| arg.contains('\0') || arg.contains("@plugin/"))
            || runtime.read_roots.len() > 64
            || runtime.env.len() > 32
        {
            return Err(error(
                "plugin_runtime_binding",
                "Approved runtime binding is invalid",
            ));
        }
        for (destination, reference) in &runtime.env {
            if !bootstrap_destination(destination) || !crate::mcp::config::valid_env_name(reference)
            {
                return Err(error(
                    "plugin_runtime_binding",
                    "Bootstrap environment must use explicit non-credential runtime references",
                ));
            }
        }
    }
    Ok(())
}

fn ceiling(
    lease: &PluginLease,
    context: &PluginRunContext,
    grant: &EnableGrant,
) -> Result<EffectiveCeiling, BoundaryError> {
    super::validate_lease_workspace(lease)?;
    let read = grant.capabilities.contains("workspace.read");
    let write = grant.capabilities.contains("workspace.write");
    Ok(EffectiveCeiling {
        capabilities: grant.capabilities.clone(),
        network_origins: if grant.capabilities.contains("network.outbound") {
            grant
                .network_origins
                .iter()
                .map(|value| parse_https(value).map(|url| url.origin().ascii_serialization()))
                .collect::<Result<BTreeSet<_>, _>>()?
        } else {
            BTreeSet::new()
        },
        credential_slots: if grant.capabilities.contains("credential.use") {
            grant.credential_slots.clone()
        } else {
            BTreeSet::new()
        },
        workspace: fs::canonicalize(&context.workspace)
            .map_err(|_| error("plugin_scope", "Plugin workspace is unavailable"))?,
        workspace_access: match (read, write) {
            (true, true) => WorkspaceAccess::ReadWrite,
            (true, false) => WorkspaceAccess::ReadOnly,
            (false, true) => WorkspaceAccess::WriteOnly,
            (false, false) => WorkspaceAccess::None,
        },
        scope: lease.record().enable_scope.clone(),
        enable_revision: lease.record().enable_revision,
    })
}
fn identity(lease: &PluginLease, entry: &str) -> PluginEntryIdentity {
    PluginEntryIdentity {
        plugin_id: lease.record().plugin_id.clone(),
        package_digest: lease.record().digest.clone(),
        entry_id: entry.into(),
    }
}
fn entry_settings(
    bindings: &PluginHostBindings,
    identity: &PluginEntryIdentity,
) -> HostEntrySettings {
    bindings
        .entries
        .get(&identity.key())
        .cloned()
        .unwrap_or_default()
}

pub(super) struct Assembler<'a> {
    pub lease: &'a Arc<PluginLease>,
    pub context: &'a PluginRunContext,
    pub bindings: &'a PluginHostBindings,
    pub pool: &'a Arc<ViewPool>,
    pub view: &'a mut Option<PluginView>,
    pub grant: &'a EnableGrant,
    pub cancel: &'a AtomicBool,
}
struct ProcessLaunch {
    argv: Vec<String>,
    cwd: PathBuf,
    policy: ProcessPolicy,
    environment: Environment,
}
impl Assembler<'_> {
    pub fn server(&mut self, entry: &McpEntry) -> Result<PluginServerDescriptor, BoundaryError> {
        let lease = self.lease;
        let context = self.context;
        let bindings = self.bindings;
        let grant = self.grant;
        let identity = identity(lease, entry.id());
        let host = entry_settings(bindings, &identity);
        host.limits.resolve()?;
        let ceiling = ceiling(lease, context, grant)?;
        let (transport, process, environment) = match entry {
            McpEntry::Stdio {
                program,
                args,
                credential_slots,
                ..
            } => {
                if !ceiling.capabilities.contains("process.exec")
                    || !ceiling.capabilities.contains("mcp.stdio")
                {
                    return Err(error(
                        "plugin_capability",
                        "Plugin stdio capability is outside its enabled ceiling",
                    ));
                }
                let ProcessLaunch {
                    argv,
                    cwd,
                    policy: process,
                    environment,
                } = self.process(program, args, credential_slots, &ceiling)?;
                (
                    McpTransportSettings::Stdio(StdioMcpSettings {
                        argv,
                        cwd: Some(cwd),
                        env: process.environment_references.clone(),
                    }),
                    Some(process),
                    environment,
                )
            }
            McpEntry::Http {
                url,
                credential_slots,
                ..
            } => {
                if !ceiling.capabilities.contains("mcp.http")
                    || !ceiling.capabilities.contains("network.outbound")
                {
                    return Err(error(
                        "plugin_capability",
                        "Plugin HTTP capability is outside its enabled ceiling",
                    ));
                }
                let endpoint = parse_https(url)?;
                let origin = endpoint.origin().ascii_serialization();
                if !ceiling.network_origins.contains(&origin) {
                    return Err(error(
                        "plugin_network_scope",
                        "Plugin endpoint origin is outside its enabled ceiling",
                    ));
                }
                let mut http = HttpMcpSettings {
                    endpoint: url.clone(),
                    allow_insecure_http: false,
                    bearer_env: None,
                    headers_env: BTreeMap::new(),
                    credential_ref: None,
                    scopes: BTreeSet::new(),
                };
                let mut selected = BTreeMap::new();
                for slot in credential_slots {
                    let binding = slot_binding(lease, slot, bindings, &ceiling)?;
                    match binding {
                        CredentialSlotBinding::HttpBearer {
                            reference,
                            audience,
                        } => {
                            audience_matches(audience, &endpoint)?;
                            if http.bearer_env.is_some() || http.credential_ref.is_some() {
                                return Err(error(
                                    "plugin_credential_scope",
                                    "Plugin HTTP bearer bindings collide",
                                ));
                            }
                            selected
                                .insert(reference.clone(), secret_reference(context, reference)?);
                            http.bearer_env = Some(reference.clone());
                        }
                        CredentialSlotBinding::HttpHeader {
                            name,
                            reference,
                            audience,
                        } => {
                            audience_matches(audience, &endpoint)?;
                            if http
                                .headers_env
                                .keys()
                                .any(|header| header.eq_ignore_ascii_case(name))
                            {
                                return Err(error(
                                    "plugin_credential_scope",
                                    "Plugin HTTP header bindings collide",
                                ));
                            }
                            selected
                                .insert(reference.clone(), secret_reference(context, reference)?);
                            http.headers_env.insert(name.clone(), reference.clone());
                        }
                        CredentialSlotBinding::HttpCredential {
                            credential_ref,
                            audience,
                            scopes,
                        } => {
                            audience_matches(audience, &endpoint)?;
                            if http.bearer_env.is_some() || http.credential_ref.is_some() {
                                return Err(error(
                                    "plugin_credential_scope",
                                    "Plugin HTTP credential bindings collide",
                                ));
                            }
                            http.credential_ref = Some(*credential_ref);
                            http.scopes = scopes.clone();
                        }
                        _ => {
                            return Err(error(
                                "plugin_credential_scope",
                                "Credential slot does not authorize this transport",
                            ));
                        }
                    }
                }
                let environment = Environment::from_pairs(
                    selected
                        .into_iter()
                        .map(|(name, value)| (name.into(), value.into())),
                );
                (McpTransportSettings::Http(http), None, environment)
            }
        };
        let settings = McpServerSettings {
            enabled: true,
            transport,
            read_only_tools: host.read_only_tools,
            limits: host.limits,
        };
        let one = McpSettings {
            servers: BTreeMap::from([(identity.server_id(), settings.clone())]),
        };
        one.validate()?;
        Ok(PluginServerDescriptor {
            server_id: identity.server_id(),
            identity,
            lease_id: lease.record().lease_id.clone(),
            capability_fingerprint: lease.record().capability_fingerprint.clone(),
            ceiling,
            settings,
            process,
            environment,
        })
    }

    pub fn hook(&mut self, entry: &HookEntry) -> Result<PluginHookDescriptor, BoundaryError> {
        let lease = self.lease;
        let context = self.context;
        let grant = self.grant;
        if !context.features.contains("hook-events-v1") {
            return Err(error("plugin_feature", "Hook feature is unavailable"));
        }
        let ceiling = ceiling(lease, context, grant)?;
        let required = if entry.kind == "check" {
            "hook.check"
        } else {
            "hook.observe"
        };
        if !ceiling.capabilities.contains("process.exec")
            || !ceiling.capabilities.contains(required)
        {
            return Err(error(
                "plugin_capability",
                "Plugin Hook capability is outside its enabled ceiling",
            ));
        }
        let ProcessLaunch {
            argv,
            cwd,
            policy: process,
            environment,
        } = self.process(&entry.program, &entry.args, &[], &ceiling)?;
        let identity = identity(lease, &entry.id);
        let hook_id = format!("sh_{}", &hash(identity.key().as_bytes())[..29]);
        Ok(PluginHookDescriptor {
            identity,
            hook_id,
            lease_id: lease.record().lease_id.clone(),
            event: entry.event.clone(),
            kind: entry.kind.clone(),
            timeout_ms: entry.timeout_ms,
            argv,
            cwd,
            ceiling,
            process,
            environment,
        })
    }

    fn process(
        &mut self,
        runtime_name: &str,
        args: &[String],
        slots: &[String],
        ceiling: &EffectiveCeiling,
    ) -> Result<ProcessLaunch, BoundaryError> {
        let lease = self.lease;
        let context = self.context;
        let bindings = self.bindings;
        let pool = self.pool;
        let view = &mut *self.view;
        let cancel = self.cancel;
        if ceiling.workspace_access == WorkspaceAccess::WriteOnly {
            return Err(error(
                "plugin_runtime_scope",
                "This process backend cannot enforce write-only workspace access",
            ));
        }
        if !context.native_isolation_available {
            return Err(error(
                "plugin_isolation_unavailable",
                "Native process isolation is unavailable for this plugin entry",
            ));
        }
        let runtime = bindings.runtimes.get(runtime_name).ok_or_else(|| {
            error(
                "plugin_runtime_missing",
                "Plugin runtime has no approved host binding",
            )
        })?;
        let program = fs::canonicalize(&runtime.program).map_err(|_| {
            error(
                "plugin_runtime_missing",
                "Approved runtime program is unavailable",
            )
        })?;
        let metadata = fs::metadata(&program).map_err(|_| {
            error(
                "plugin_runtime_missing",
                "Approved runtime metadata is unavailable",
            )
        })?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return Err(error(
                "plugin_runtime_missing",
                "Approved runtime must be an executable regular file",
            ));
        }
        let mut runtime_roots = Vec::new();
        for root in &runtime.read_roots {
            let root = fs::canonicalize(root).map_err(|_| {
                error(
                    "plugin_runtime_missing",
                    "Approved runtime read root is unavailable",
                )
            })?;
            if !root.is_dir() {
                return Err(error(
                    "plugin_runtime_scope",
                    "Runtime read roots must be directories",
                ));
            }
            runtime_roots.push(root);
        }
        for path in runtime_roots.iter().chain(std::iter::once(&program)) {
            if view::overlaps(path, pool.path())? {
                return Err(error(
                    "plugin_runtime_scope",
                    "Runtime bootstrap cannot expose the view pool",
                ));
            }
            if view::overlaps(path, &ceiling.workspace)?
                && ceiling.workspace_access != WorkspaceAccess::ReadOnly
            {
                return Err(error(
                    "plugin_runtime_scope",
                    "Runtime bootstrap overlaps an unapproved or writable workspace",
                ));
            }
            for protected in &context.protected_roots {
                if view::overlaps(path, &view::resolve(protected)?)? {
                    return Err(error(
                        "plugin_runtime_scope",
                        "Runtime bootstrap addresses protected host data",
                    ));
                }
            }
        }
        let mut references = runtime.env.clone();
        let mut values = BTreeMap::new();
        for reference in runtime.env.values() {
            let value = environment_reference(context, reference)?;
            if crate::redaction::contains_secret(&value, &context.secrets.snapshot()?) {
                return Err(error(
                    "plugin_credential_scope",
                    "Protected environment values require declared credential slots",
                ));
            }
            values.insert(reference.clone(), value);
        }
        for slot in slots {
            let CredentialSlotBinding::Environment {
                destination,
                reference,
            } = slot_binding(lease, slot, bindings, ceiling)?
            else {
                return Err(error(
                    "plugin_credential_scope",
                    "Credential slot does not authorize this process transport",
                ));
            };
            if !crate::mcp::config::valid_env_name(destination)
                || !crate::mcp::config::valid_env_name(reference)
                || matches!(destination.as_str(), "HOME" | "TMPDIR")
                || references.contains_key(destination)
            {
                return Err(error(
                    "plugin_credential_scope",
                    "Process credential destination is invalid or collides",
                ));
            }
            values.insert(reference.clone(), secret_reference(context, reference)?);
            references.insert(destination.clone(), reference.clone());
        }
        cancelled(cancel)?;
        if view.is_none() {
            *view = Some(pool.materialize(lease.package(), lease.record(), cancel)?);
        }
        let materialized = view
            .as_ref()
            .ok_or_else(|| error("plugin_view", "Plugin package view is unavailable"))?;
        materialized.verify()?;
        let identity = materialized.identity().clone();
        let private_ref = format!(
            "SAYA_PLUGIN_PRIVATE_{}",
            hash(lease.record().lease_id.as_bytes())[..24].to_ascii_uppercase()
        );
        if context.environment.get_os(&private_ref).is_some() || values.contains_key(&private_ref) {
            return Err(error(
                "plugin_environment",
                "Generated private environment reference collides",
            ));
        }
        let scratch = identity
            .scratch
            .to_str()
            .ok_or_else(|| error("plugin_view", "Plugin scratch path is not UTF-8"))?
            .to_owned();
        references.insert("HOME".into(), private_ref.clone());
        references.insert("TMPDIR".into(), private_ref.clone());
        values.insert(private_ref, scratch);
        let mut argv = vec![
            program
                .to_str()
                .ok_or_else(|| error("plugin_runtime_scope", "Runtime program path must be UTF-8"))?
                .into(),
        ];
        argv.extend(runtime.args_prefix.iter().cloned());
        for argument in args {
            if let Some(relative) = argument.strip_prefix("@plugin/") {
                crate::plugin_manifest::validate_relative_path(relative)?;
                let exists = lease.package().file(relative).is_some()
                    || lease
                        .package()
                        .files()
                        .iter()
                        .any(|file| file.path().starts_with(&format!("{relative}/")));
                if !exists {
                    return Err(error(
                        "plugin_view",
                        "Plugin argument addresses missing sealed content",
                    ));
                }
                argv.push(
                    identity
                        .package
                        .join(relative)
                        .to_str()
                        .ok_or_else(|| error("plugin_view", "Plugin argument path must be UTF-8"))?
                        .into(),
                );
            } else {
                if argument.contains("@plugin/") {
                    return Err(error(
                        "plugin_view",
                        "Plugin path placeholders must occupy a complete argv element",
                    ));
                }
                argv.push(argument.clone());
            }
        }
        let mut read_roots = runtime_roots;
        read_roots.push(identity.package.clone());
        let mut write_roots = vec![identity.scratch.clone()];
        let cwd = if ceiling.workspace_access == WorkspaceAccess::None {
            identity.scratch.clone()
        } else {
            read_roots.push(ceiling.workspace.clone());
            ceiling.workspace.clone()
        };
        if ceiling.workspace_access == WorkspaceAccess::ReadWrite {
            write_roots.push(ceiling.workspace.clone());
        }
        let policy = ProcessPolicy {
            runtime_name: runtime_name.into(),
            program,
            read_roots,
            write_roots,
            protected_roots: context.protected_roots.clone(),
            package_view: identity,
            environment_references: references,
            inherit_host_environment: false,
            native_isolation_required: true,
        };
        let environment = Environment::from_pairs(
            values
                .into_iter()
                .map(|(name, value)| (OsString::from(name), OsString::from(value))),
        );
        Ok(ProcessLaunch {
            argv,
            cwd,
            policy,
            environment,
        })
    }
}
fn slot_binding<'a>(
    lease: &PluginLease,
    slot: &str,
    bindings: &'a PluginHostBindings,
    ceiling: &EffectiveCeiling,
) -> Result<&'a CredentialSlotBinding, BoundaryError> {
    if !ceiling.capabilities.contains("credential.use") || !ceiling.credential_slots.contains(slot)
    {
        return Err(error(
            "plugin_credential_scope",
            "Credential slot is outside the enabled plugin ceiling",
        ));
    }
    bindings
        .slots
        .get(&slot_key(
            &lease.record().plugin_id,
            &lease.record().digest,
            slot,
        ))
        .ok_or_else(|| {
            error(
                "plugin_credential_missing",
                "Plugin credential slot has no exact host binding",
            )
        })
}
fn environment_reference(
    context: &PluginRunContext,
    reference: &str,
) -> Result<String, BoundaryError> {
    if !crate::mcp::config::valid_env_name(reference) {
        return Err(error(
            "plugin_environment",
            "Plugin environment reference name is invalid",
        ));
    }
    context
        .environment
        .get_os(reference)
        .and_then(std::ffi::OsStr::to_str)
        .filter(|value| !value.is_empty() && value.len() <= 64 * 1024 && !value.contains('\0'))
        .map(str::to_owned)
        .ok_or_else(|| {
            error(
                "plugin_environment",
                "Selected plugin environment reference is unavailable",
            )
        })
}
fn secret_reference(context: &PluginRunContext, reference: &str) -> Result<String, BoundaryError> {
    let value = environment_reference(context, reference)?;
    context.secrets.register(std::slice::from_ref(&value))?;
    Ok(value)
}
fn parse_https(value: &str) -> Result<reqwest::Url, BoundaryError> {
    let url = reqwest::Url::parse(value)
        .map_err(|_| error("plugin_network_scope", "Plugin endpoint is invalid"))?;
    if value.len() > 4096
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || value.chars().any(crate::text::is_terminal_control)
    {
        return Err(error(
            "plugin_network_scope",
            "Plugin endpoint must use HTTPS without embedded credentials",
        ));
    }
    Ok(url)
}
fn audience_matches(audience: &str, endpoint: &reqwest::Url) -> Result<(), BoundaryError> {
    if parse_https(audience)? != *endpoint {
        return Err(error(
            "plugin_credential_scope",
            "Credential purpose is bound to another endpoint",
        ));
    }
    Ok(())
}
fn bootstrap_destination(name: &str) -> bool {
    matches!(
        name,
        "PATH"
            | "LANG"
            | "LC_ALL"
            | "LC_CTYPE"
            | "TZ"
            | "PYTHONPATH"
            | "PYTHONHOME"
            | "PYTHONDONTWRITEBYTECODE"
            | "PYTHONIOENCODING"
            | "NODE_PATH"
            | "NODE_NO_WARNINGS"
    )
}
