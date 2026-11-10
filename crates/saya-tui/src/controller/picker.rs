//! Explicit selectors for discovered Skills, MCP servers and installed Plugins.
use super::*;
use crate::view::{Details, PickerItem, PickerKind, PickerPanel};
use saya_app::interaction::RunCancellation;
use saya_core::{
    config::Config,
    contract::ErrorInfo,
    credentials::SecretRegistry,
    mcp::config::McpTransportSettings,
    plugin_store::{PluginStore, StoreLimits},
    project_context::ProjectContext,
};
use std::sync::Arc;

fn failure(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}

fn panel(kind: PickerKind, title: &str, items: Vec<PickerItem>, empty_hint: &str) -> PickerPanel {
    PickerPanel {
        kind,
        title: title.into(),
        items,
        selected: 0,
        scroll: 0,
        page_rows: 0,
        hint: empty_hint.into(),
    }
}

fn load_skills(
    config: &Config,
    secrets: Arc<SecretRegistry>,
) -> std::result::Result<PickerPanel, BoundaryError> {
    let project = ProjectContext::new(&config.cwd, config.home().as_deref())?;
    let snapshot = project.snapshot(None)?;
    let mut items = snapshot
        .skills
        .into_iter()
        .map(|skill| PickerItem {
            id: format!("{}:{}", skill.name, skill.sha256),
            label: skill.name,
            description: skill.description,
            details: vec![
                format!("Source {}", skill.path.display()),
                format!("Scope {}", skill.scope),
                format!("SHA-256 {}", skill.sha256),
                "Selection opens details. Use the Agent Skill tools to activate context; selection grants no tools or permissions.".into(),
            ],
            active: false,
        })
        .collect::<Vec<_>>();
    let plugin_root = config.data_dir.join("plugins");
    if plugin_root.try_exists().map_err(|_| {
        failure(
            "plugin_io",
            "Plugin store availability could not be checked",
        )
    })? {
        saya_app::session::check_roots(config, &config.cwd, config.mode).map_err(|_| {
            failure(
                "plan_read_only",
                "PLAN plugin storage must be outside the workspace",
            )
        })?;
        let store = PluginStore::open(&plugin_root, secrets, StoreLimits::default())?;
        for summary in store.list()?.into_iter().filter(|summary| summary.selected) {
            let inspection = store.inspect(&summary.digest)?;
            for skill in inspection.manifest.skills {
                let label = format!("{}/{}", summary.plugin_id, skill.id);
                items.push(PickerItem {
                    id: format!("{label}:{}", summary.digest),
                    label,
                    description: inspection.manifest.plugin.description.clone(),
                    details: vec![
                        format!("Plugin {}", summary.plugin_id),
                        format!("Version {}", summary.version),
                        format!("Package SHA-256 {}", summary.digest),
                        format!(
                            "Enabled scopes {}",
                            summary
                                .enabled_scopes
                                .iter()
                                .filter(|record| record.enabled)
                                .count()
                        ),
                        "Selection opens details. Plugin enablement and current permissions remain authoritative."
                            .into(),
                    ],
                    active: summary.enabled_scopes.iter().any(|record| record.enabled),
                });
            }
        }
    }
    items.sort_by(|left, right| left.label.cmp(&right.label));
    Ok(panel(
        PickerKind::Skills,
        "Skills",
        items,
        "No applicable Skills were discovered.",
    ))
}

fn load_plugins(
    config: &Config,
    secrets: Arc<SecretRegistry>,
) -> std::result::Result<PickerPanel, BoundaryError> {
    saya_app::session::check_roots(config, &config.cwd, config.mode).map_err(|_| {
        failure(
            "plan_read_only",
            "PLAN plugin storage must be outside the workspace",
        )
    })?;
    let root = config.data_dir.join("plugins");
    if !root.try_exists().map_err(|_| {
        failure(
            "plugin_io",
            "Plugin store availability could not be checked",
        )
    })? {
        return Ok(panel(
            PickerKind::Plugins,
            "Plugins",
            vec![],
            "No Plugin store exists. Install a local package with `saya plugin install`.",
        ));
    }
    let store = PluginStore::open(root, secrets, StoreLimits::default())?;
    let mut items = store
        .list()?
        .into_iter()
        .map(|summary| {
            let enabled = summary.enabled_scopes.iter().any(|record| record.enabled);
            PickerItem {
                id: summary.digest.clone(),
                label: summary.plugin_id.clone(),
                description: format!(
                    "v{} · {:?}{}",
                    summary.version,
                    summary.state,
                    if summary.selected { " · selected" } else { "" }
                ),
                details: vec![
                    format!("Plugin {}", summary.plugin_id),
                    format!("Version {}", summary.version),
                    format!("State {:?}", summary.state),
                    format!("Selected {}", summary.selected),
                    format!("SHA-256 {}", summary.digest),
                    format!("Enabled scopes {}", summary.enabled_scopes.len()),
                    format!(
                        "Requested capabilities {} · network origins {} · credential slots {}",
                        summary.requested_capabilities.requested.len(),
                        summary.requested_capabilities.network_origins.len(),
                        summary.requested_capabilities.credential_slots.len()
                    ),
                    "Selection opens details. Use `saya plugin enable|disable` for explicit capability grants."
                        .into(),
                ],
                active: enabled,
            }
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| left.label.cmp(&right.label));
    Ok(panel(
        PickerKind::Plugins,
        "Plugins",
        items,
        "No installed Plugins are available.",
    ))
}

fn mcp_panel(config: &Config) -> PickerPanel {
    let items = config
        .mcp
        .servers
        .iter()
        .map(|(name, server)| {
            let transport = match &server.transport {
                McpTransportSettings::Stdio(settings) => {
                    format!("stdio · {} argv", settings.argv.len())
                }
                McpTransportSettings::Http(_) => "HTTP".into(),
            };
            PickerItem {
                id: name.clone(),
                label: name.clone(),
                description: format!(
                    "{} · {}",
                    if server.enabled { "enabled" } else { "disabled" },
                    transport
                ),
                details: vec![
                    format!("Server {name}"),
                    format!("Transport {transport}"),
                    format!("Read-only declarations {}", server.read_only_tools.len()),
                    "Enter toggles enablement for subsequent runs. Existing approvals and permissions are not reused."
                        .into(),
                ],
                active: server.enabled,
            }
        })
        .collect();
    panel(
        PickerKind::Mcps,
        "MCP servers",
        items,
        "No MCP servers are configured.",
    )
}

impl App {
    pub(super) async fn open_picker(&mut self, kind: &str) -> Result<()> {
        self.require_idle_configuration()?;
        self.state.command_palette = None;
        self.state.completion.dismiss();
        match kind {
            "mcps" => {
                self.state.picker_panel = Some(self.safe_picker(mcp_panel(&self.config)));
            }
            "skills" | "plugins" => {
                let config = self.config.clone();
                let secrets = self.secret_registry.clone();
                let plugin = kind == "plugins";
                let cancel = Arc::new(RunCancellation::new());
                let checking = cancel.clone();
                let join = tokio::task::spawn_blocking(move || {
                    if checking.is_cancelled() {
                        return Err(failure("cancelled", "Selector loading cancelled"));
                    }
                    let panel = if plugin {
                        load_plugins(&config, secrets)?
                    } else {
                        load_skills(&config, secrets)?
                    };
                    Ok(services::UiResult::Picker(panel))
                });
                self.begin_ui(cancel, join);
            }
            _ => return Err("Unknown selector".into()),
        }
        Ok(())
    }

    pub(super) async fn picker_key(&mut self, key: KeyEvent) -> Result<bool> {
        if key.kind != KeyEventKind::Press && matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') | KeyCode::Char('d') => {
                    self.state.picker_panel = None;
                    return Ok(false);
                }
                _ => return Ok(false),
            }
        }
        match key.code {
            KeyCode::Esc => self.state.picker_panel = None,
            KeyCode::Up => {
                if let Some(panel) = &mut self.state.picker_panel {
                    panel.selected = panel.selected.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                if let Some(panel) = &mut self.state.picker_panel {
                    panel.selected = panel
                        .selected
                        .saturating_add(1)
                        .min(panel.items.len().saturating_sub(1));
                }
            }
            KeyCode::PageUp => {
                if let Some(panel) = &mut self.state.picker_panel {
                    panel.selected = panel.selected.saturating_sub(panel.page_rows.max(1));
                }
            }
            KeyCode::PageDown => {
                if let Some(panel) = &mut self.state.picker_panel {
                    panel.selected = panel
                        .selected
                        .saturating_add(panel.page_rows.max(1))
                        .min(panel.items.len().saturating_sub(1));
                }
            }
            KeyCode::Home => {
                if let Some(panel) = &mut self.state.picker_panel {
                    panel.selected = 0;
                }
            }
            KeyCode::End => {
                if let Some(panel) = &mut self.state.picker_panel {
                    panel.selected = panel.items.len().saturating_sub(1);
                }
            }
            KeyCode::Enter => self.select_picker_item()?,
            _ => {}
        }
        Ok(false)
    }

    fn select_picker_item(&mut self) -> Result<()> {
        let Some(panel) = &self.state.picker_panel else {
            return Ok(());
        };
        let Some(item) = panel.items.get(panel.selected).cloned() else {
            return Ok(());
        };
        match panel.kind {
            PickerKind::Skills | PickerKind::Plugins => {
                let title = item.label.clone();
                self.state.picker_panel = None;
                self.state.set_details(Details {
                    title: format!("{title} · selected"),
                    lines: item.details,
                });
                self.state.details_scroll = 0;
            }
            PickerKind::Mcps => self.toggle_mcp(&item)?,
        }
        Ok(())
    }

    fn toggle_mcp(&mut self, item: &PickerItem) -> Result<()> {
        let server = self
            .config
            .mcp
            .servers
            .get_mut(&item.id)
            .ok_or("Selected MCP server is unavailable")?;
        let previous = server.enabled;
        server.enabled = !previous;
        if let Err(error) = self.refresh_secret_inputs() {
            if let Some(server) = self.config.mcp.servers.get_mut(&item.id) {
                server.enabled = previous;
            }
            return Err(Box::new(error));
        }
        let enabled = !previous;
        self.reset_mcp_statuses();
        if let Some(panel) = &mut self.state.picker_panel
            && let Some(item) = panel.items.get_mut(panel.selected)
        {
            item.active = enabled;
            item.description = item.description.replacen(
                if previous { "enabled" } else { "disabled" },
                if enabled { "enabled" } else { "disabled" },
                1,
            );
        }
        self.notice(&format!(
            "MCP server {} {} for subsequent runs.",
            self.safe(&item.id),
            if enabled { "enabled" } else { "disabled" }
        ));
        Ok(())
    }
}
