//! Explicit selectors for discovered Skills, MCP servers and installed Plugins.
use super::*;
use crate::i18n::{common, picker as text, services as service_text};
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
                format!("{} {}", text::SOURCE, skill.path.display()),
                format!("{} {}", text::SCOPE, skill.scope),
                format!("{} {}", text::SHA256, skill.sha256),
                text::SKILL_DETAIL_HINT.into(),
            ],
            active: false,
        })
        .collect::<Vec<_>>();
    let plugin_root = config.data_dir.join("plugins");
    if plugin_root
        .try_exists()
        .map_err(|_| failure("plugin_io", text::PLUGIN_STORE_CHECK))?
    {
        saya_app::session::check_roots(config, &config.cwd, config.mode)
            .map_err(|_| failure("plan_read_only", text::PLAN_PLUGIN_STORAGE))?;
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
                        format!("{} {}", text::PLUGIN, summary.plugin_id),
                        format!("{} {}", text::VERSION, summary.version),
                        format!("{} {}", text::PACKAGE_SHA256, summary.digest),
                        format!(
                            "{} {}",
                            text::ENABLED_SCOPES,
                            summary
                                .enabled_scopes
                                .iter()
                                .filter(|record| record.enabled)
                                .count()
                        ),
                        text::PLUGIN_DETAIL_HINT.into(),
                    ],
                    active: summary.enabled_scopes.iter().any(|record| record.enabled),
                });
            }
        }
    }
    items.sort_by(|left, right| left.label.cmp(&right.label));
    Ok(panel(
        PickerKind::Skills,
        text::SKILLS_TITLE,
        items,
        text::NO_SKILLS,
    ))
}

fn load_plugins(
    config: &Config,
    secrets: Arc<SecretRegistry>,
) -> std::result::Result<PickerPanel, BoundaryError> {
    saya_app::session::check_roots(config, &config.cwd, config.mode)
        .map_err(|_| failure("plan_read_only", text::PLAN_PLUGIN_STORAGE))?;
    let root = config.data_dir.join("plugins");
    if !root
        .try_exists()
        .map_err(|_| failure("plugin_io", text::PLUGIN_STORE_CHECK))?
    {
        return Ok(panel(
            PickerKind::Plugins,
            text::PLUGINS_TITLE,
            vec![],
            text::PLUGIN_STORE_MISSING,
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
                    if summary.selected {
                        format!(" · {}", text::SELECTED)
                    } else {
                        String::new()
                    }
                ),
                details: vec![
                    format!("{} {}", text::PLUGIN, summary.plugin_id),
                    format!("{} {}", text::VERSION, summary.version),
                    format!("{} {:?}", text::STATE, summary.state),
                    format!("{} {}", text::SELECTED, summary.selected),
                    format!("{} {}", text::SHA256, summary.digest),
                    format!("{} {}", text::ENABLED_SCOPES, summary.enabled_scopes.len()),
                    format!(
                        "{} {} · {} {} · {} {}",
                        text::REQUESTED_CAPABILITIES,
                        summary.requested_capabilities.requested.len(),
                        text::NETWORK_ORIGINS,
                        summary.requested_capabilities.network_origins.len(),
                        text::CREDENTIAL_SLOTS,
                        summary.requested_capabilities.credential_slots.len()
                    ),
                    text::PLUGIN_CLI_HINT.into(),
                ],
                active: enabled,
            }
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| left.label.cmp(&right.label));
    Ok(panel(
        PickerKind::Plugins,
        text::PLUGINS_TITLE,
        items,
        text::NO_PLUGINS,
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
                    format!(
                        "{} · {} {}",
                        common::TRANSPORT_STDIO,
                        settings.argv.len(),
                        text::ARGV
                    )
                }
                McpTransportSettings::Http(_) => common::TRANSPORT_HTTP.into(),
            };
            PickerItem {
                id: name.clone(),
                label: name.clone(),
                description: format!(
                    "{} · {}",
                    if server.enabled {
                        text::ENABLED
                    } else {
                        text::DISABLED
                    },
                    transport
                ),
                details: vec![
                    format!("{} {name}", text::SERVER),
                    format!("{} {transport}", text::TRANSPORT),
                    format!(
                        "{} {}",
                        text::READ_ONLY_DECLARATIONS,
                        server.read_only_tools.len()
                    ),
                    text::MCP_DETAIL_HINT.into(),
                ],
                active: server.enabled,
            }
        })
        .collect();
    panel(PickerKind::Mcps, text::MCPS_TITLE, items, text::NO_MCPS)
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
                        return Err(failure("cancelled", service_text::SELECTOR_CANCELLED));
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
            _ => return Err(text::UNKNOWN_SELECTOR.into()),
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
            KeyCode::Enter => self.select_picker_item().await?,
            _ => {}
        }
        Ok(false)
    }

    async fn select_picker_item(&mut self) -> Result<()> {
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
                    title: text::selected_title(&title),
                    lines: item.details,
                });
                self.state.details_scroll = 0;
            }
            PickerKind::Mcps => self.toggle_mcp(&item)?,
            PickerKind::Sessions => {
                self.state.picker_panel = None;
                if self
                    .prepared
                    .as_ref()
                    .is_some_and(|prepared| prepared.session.id.0.to_string() == item.id)
                {
                    self.notice(service_text::SESSION_ALREADY_OPEN);
                } else {
                    self.service("/resume", &item.id).await?;
                }
            }
        }
        Ok(())
    }

    fn toggle_mcp(&mut self, item: &PickerItem) -> Result<()> {
        let server = self
            .config
            .mcp
            .servers
            .get_mut(&item.id)
            .ok_or(text::MCP_UNAVAILABLE)?;
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
                if previous {
                    text::ENABLED
                } else {
                    text::DISABLED
                },
                if enabled {
                    text::ENABLED
                } else {
                    text::DISABLED
                },
                1,
            );
        }
        self.notice(&text::mcp_changed(&self.safe(&item.id), enabled));
        Ok(())
    }
}
