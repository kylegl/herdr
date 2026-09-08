use std::fs;
use std::io::{self, Write};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::layout::Direction;

use super::{App, Mode};

static NEXT_COMMAND_NAMESPACE: AtomicU64 = AtomicU64::new(1);

pub(super) fn new_command_namespace() -> String {
    let counter = NEXT_COMMAND_NAMESPACE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}-{counter}", std::process::id())
}

#[derive(Debug)]
pub(super) struct EndpointCommandRegistry {
    entries: Vec<EndpointCommand>,
}

#[derive(Debug)]
struct EndpointCommand {
    id: String,
    binding: crate::config::CustomCommandKeybind,
    action: crate::protocol::ClientShellCommandAction,
}

impl EndpointCommandRegistry {
    pub(super) fn new(bindings: &[crate::config::CustomCommandKeybind]) -> Self {
        let namespace = new_command_namespace();
        let entries = bindings
            .iter()
            .enumerate()
            .map(|(index, binding)| EndpointCommand {
                action: binding.action.into(),
                id: format!("cmd_{namespace}_{index}"),
                binding: binding.clone(),
            })
            .collect();
        Self { entries }
    }
}

impl App {
    pub(crate) fn client_shell_keybindings_profile(&self) -> Option<&str> {
        self.client_shell_keybindings_profile.as_deref()
    }

    pub(crate) fn client_shell_command_manifest(&self) -> Vec<crate::protocol::ClientShellCommand> {
        self.endpoint_commands
            .entries
            .iter()
            .map(|entry| crate::protocol::ClientShellCommand {
                command_id: entry.id.clone(),
                binding_label: entry.binding.label.clone(),
                binding_labels: entry.binding.bindings.labels(),
                action: entry.action,
                description: entry.binding.description.clone(),
            })
            .collect()
    }

    pub(crate) fn resolve_client_shell_command(
        &self,
        id: &str,
    ) -> Option<crate::config::CustomCommandKeybind> {
        self.endpoint_commands
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.binding.clone())
    }

    pub(crate) fn handle_command_invoke(
        &mut self,
        id: String,
        params: crate::api::schema::CommandInvokeParams,
    ) -> String {
        let Some(binding) = self.resolve_client_shell_command(&params.command_id) else {
            return crate::app::api::responses::encode_error(
                id,
                "command_not_found",
                "custom command manifest is stale; reload configuration",
            );
        };
        if let Err((code, message)) = self.focus_client_shell_command_target(&params) {
            return crate::app::api::responses::encode_error(id, code, message);
        }
        let selected_text = if binding.action == crate::config::CustomCommandAction::PluginAction {
            let Some(selection) = params.selection.as_ref() else {
                return self.execute_custom_command_response(id, &binding, None);
            };
            if params.pane_id.as_deref() != Some(selection.pane_id.as_str()) {
                return crate::app::api::responses::encode_error(
                    id,
                    "command_target_mismatch",
                    "command selection does not belong to the requested pane",
                );
            }
            match self.pane_selection_text(selection) {
                Ok(text) => Some(text),
                Err((code, message)) => {
                    return crate::app::api::responses::encode_error(id, code, message);
                }
            }
        } else {
            None
        };
        self.execute_custom_command_response(id, &binding, selected_text)
    }

    fn execute_custom_command_response(
        &mut self,
        id: String,
        binding: &crate::config::CustomCommandKeybind,
        selected_text: Option<String>,
    ) -> String {
        match self.execute_custom_command_binding(binding, selected_text) {
            Ok(()) => crate::app::api::responses::encode_success(
                id,
                crate::api::schema::ResponseResult::Ok {},
            ),
            Err(error) => {
                crate::app::api::responses::encode_error(id, "command_failed", error.to_string())
            }
        }
    }

    fn focus_client_shell_command_target(
        &mut self,
        params: &crate::api::schema::CommandInvokeParams,
    ) -> Result<(), (&'static str, String)> {
        if let Some(pane_id) = params.pane_id.as_deref() {
            let Some((workspace_index, pane)) = self.parse_pane_id(pane_id) else {
                return Err(("pane_not_found", format!("pane not found: {pane_id}")));
            };
            let Some(tab_index) =
                self.state.workspaces[workspace_index].find_tab_index_for_pane(pane)
            else {
                return Err(("pane_not_found", format!("pane not found: {pane_id}")));
            };
            self.validate_client_shell_command_parent_ids(params, workspace_index, tab_index)?;
            self.state.focus_pane_in_workspace(workspace_index, pane);
            return Ok(());
        }

        if let Some(tab_id) = params.tab_id.as_deref() {
            let Some((workspace_index, tab_index)) = self.parse_tab_id(tab_id) else {
                return Err(("tab_not_found", format!("tab not found: {tab_id}")));
            };
            self.validate_client_shell_command_workspace_id(params, workspace_index)?;
            self.state.switch_workspace_tab(workspace_index, tab_index);
            return Ok(());
        }

        if let Some(workspace_id) = params.workspace_id.as_deref() {
            let Some(workspace_index) = self.parse_workspace_id(workspace_id) else {
                return Err((
                    "workspace_not_found",
                    format!("workspace not found: {workspace_id}"),
                ));
            };
            self.state.switch_workspace(workspace_index);
        }
        Ok(())
    }

    fn validate_client_shell_command_parent_ids(
        &self,
        params: &crate::api::schema::CommandInvokeParams,
        workspace_index: usize,
        tab_index: usize,
    ) -> Result<(), (&'static str, String)> {
        self.validate_client_shell_command_workspace_id(params, workspace_index)?;
        if let Some(tab_id) = params.tab_id.as_deref() {
            let actual = self
                .public_tab_id(workspace_index, tab_index)
                .unwrap_or_default();
            if tab_id != actual {
                return Err((
                    "command_target_mismatch",
                    "command pane does not belong to the requested tab".to_owned(),
                ));
            }
        }
        Ok(())
    }

    fn validate_client_shell_command_workspace_id(
        &self,
        params: &crate::api::schema::CommandInvokeParams,
        workspace_index: usize,
    ) -> Result<(), (&'static str, String)> {
        if params
            .workspace_id
            .as_deref()
            .is_some_and(|workspace_id| workspace_id != self.public_workspace_id(workspace_index))
        {
            return Err((
                "command_target_mismatch",
                "command target does not belong to the requested workspace".to_owned(),
            ));
        }
        Ok(())
    }
    pub(crate) fn execute_custom_command_binding(
        &mut self,
        binding: &crate::config::CustomCommandKeybind,
        selected_text: Option<String>,
    ) -> io::Result<()> {
        match binding.action {
            crate::config::CustomCommandAction::Shell => self.spawn_custom_command(binding),
            crate::config::CustomCommandAction::Pane => {
                self.spawn_pane_command(&binding.command, Vec::new())
            }
            crate::config::CustomCommandAction::Popup => self.spawn_custom_popup_command(binding),
            crate::config::CustomCommandAction::PluginAction => self
                .invoke_plugin_action_from_keybind(binding.command.clone(), selected_text)
                .map_err(io::Error::other),
        }
    }

    fn spawn_custom_popup_command(
        &mut self,
        binding: &crate::config::CustomCommandKeybind,
    ) -> io::Result<()> {
        self.spawn_popup_shell_command(
            &binding.command,
            None,
            self.custom_command_env().0,
            crate::app::popup::PopupGeometry {
                width: binding.width,
                height: binding.height,
            },
        )
    }

    pub(crate) fn custom_command_env(&self) -> (Vec<(String, String)>, Option<std::path::PathBuf>) {
        let mut env = vec![(
            crate::api::SOCKET_PATH_ENV_VAR.to_string(),
            crate::api::socket_path().display().to_string(),
        )];
        if let Ok(current_exe) = std::env::current_exe() {
            env.push((
                "HERDR_BIN_PATH".to_string(),
                current_exe.display().to_string(),
            ));
        }

        let mut cwd = None;
        if let Some(ws_idx) = self.state.active {
            env.push((
                "HERDR_ACTIVE_WORKSPACE_ID".to_string(),
                self.public_workspace_id(ws_idx),
            ));
            if let Some(workspace) = self.state.workspaces.get(ws_idx) {
                let tab_idx = workspace.active_tab_index();
                if let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) {
                    env.push(("HERDR_ACTIVE_TAB_ID".to_string(), tab_id));
                }
                if let Some(pane_id) = workspace.focused_pane_id() {
                    if let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) {
                        env.push(("HERDR_ACTIVE_PANE_ID".to_string(), public_pane_id));
                    }
                    if let Some(pane_cwd) = workspace.active_tab().and_then(|tab| {
                        tab.cwd_for_pane(pane_id, &self.state.terminals, &self.terminal_runtimes)
                    }) {
                        env.push((
                            "HERDR_ACTIVE_PANE_CWD".to_string(),
                            pane_cwd.display().to_string(),
                        ));
                        if pane_cwd.is_dir() {
                            cwd = Some(pane_cwd);
                        }
                    }
                }
            }
        }
        (env, cwd)
    }

    fn spawn_custom_command(
        &mut self,
        binding: &crate::config::CustomCommandKeybind,
    ) -> std::io::Result<()> {
        let mut command = crate::platform::detached_custom_command_process(&binding.command);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let (env, cwd) = self.custom_command_env();
        command.envs(env);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let child = command.spawn()?;
        self.detached_process_children.push(child);
        Ok(())
    }

    pub(crate) fn open_focused_scrollback_in_editor(&mut self) -> std::io::Result<()> {
        let ws_idx = self
            .state
            .active
            .ok_or_else(|| std::io::Error::other("no active workspace"))?;
        let ws = self
            .state
            .workspaces
            .get(ws_idx)
            .ok_or_else(|| std::io::Error::other("active workspace disappeared"))?;
        let pane_id = ws
            .focused_pane_id()
            .ok_or_else(|| std::io::Error::other("no focused pane"))?;
        let scrollback = self
            .state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            .ok_or_else(|| std::io::Error::other("focused pane has no scrollback runtime"))?
            .recent_unwrapped_text_snapshot(usize::MAX)
            .text;

        let path = write_scrollback_temp_file(&scrollback)?;

        let argv = match crate::platform::scrollback_editor_argv(&path) {
            Ok(argv) => argv,
            Err(err) => {
                let _ = fs::remove_file(&path);
                return Err(err);
            }
        };
        let (env, _) = self.custom_command_env();
        let new_pane = match self.spawn_overlay_argv_command(&argv, None, env, vec![path.clone()]) {
            Ok((_, new_pane)) => new_pane,
            Err(err) => {
                let _ = fs::remove_file(&path);
                return Err(err);
            }
        };
        let terminal_id = new_pane.terminal.id.clone();
        self.terminal_runtimes
            .insert(terminal_id.clone(), new_pane.runtime);
        self.state
            .remove_alias_shadowed_by_new_pane(new_pane.pane_id);
        self.state.terminals.insert(terminal_id, new_pane.terminal);

        if let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) {
            self.state.toast = Some(crate::app::state::ToastNotification {
                kind: crate::app::state::ToastKind::Finished,
                title: "opened scrollback".to_string(),
                context: format!("focused pane {public_pane_id}"),
                position: None,
                target: None,
            });
        }
        Ok(())
    }

    fn spawn_pane_command(
        &mut self,
        command: &str,
        temp_files: Vec<std::path::PathBuf>,
    ) -> std::io::Result<()> {
        let target = self
            .current_plugin_context("custom-command-pane")
            .focused_pane_id;
        // Canonicalize the dock exchange before the workspace allocates a public pane number.
        self.state.prepare_attention_topology_mutation();
        let result = self.spawn_pane_command_after_attention_canonicalization(
            command,
            temp_files,
            target.as_deref(),
        );
        if result.is_err() {
            self.state
                .reconcile_attention_dock_from(&self.terminal_runtimes);
        }
        result
    }

    fn spawn_pane_command_after_attention_canonicalization(
        &mut self,
        command: &str,
        temp_files: Vec<std::path::PathBuf>,
        target_pane_id: Option<&str>,
    ) -> std::io::Result<()> {
        let Some((ws_idx, target_pane)) = target_pane_id
            .and_then(|pane_id| self.parse_pane_id(pane_id))
            .or_else(|| {
                let ws_idx = self.state.active?;
                Some((
                    ws_idx,
                    self.state.workspaces.get(ws_idx)?.focused_pane_id()?,
                ))
            })
        else {
            return Err(std::io::Error::other("no active pane"));
        };
        self.state.focus_pane_in_workspace(ws_idx, target_pane);
        let previous_focus_target = self.state.current_pane_focus_target();
        let (rows, cols) = self.state.estimate_pane_size();
        let new_rows = rows.max(4);
        let new_cols = cols.max(10);
        let (env, _) = self.custom_command_env();

        let ws = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .ok_or_else(|| std::io::Error::other("active workspace disappeared"))?;
        let tab_idx = ws.active_tab_index();
        let previous_focus = ws
            .focused_pane_id()
            .ok_or_else(|| std::io::Error::other("no focused pane"))?;
        let previous_zoomed = ws.active_tab().map(|tab| tab.zoomed).unwrap_or(false);
        let cwd = ws.active_tab().and_then(|tab| {
            tab.cwd_for_pane(
                previous_focus,
                &self.state.terminals,
                &self.terminal_runtimes,
            )
        });
        let new_pane = ws.split_focused_command(
            Direction::Horizontal,
            new_rows,
            new_cols,
            cwd,
            command,
            env,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.state.host_terminal_appearance,
        )?;
        let new_pane_id = new_pane.pane_id;
        self.terminal_runtimes
            .insert(new_pane.terminal.id.clone(), new_pane.runtime);
        self.state
            .terminals
            .insert(new_pane.terminal.id.clone(), new_pane.terminal);
        let new_focus_target = crate::app::state::PaneFocusTarget {
            workspace_id: ws.id.clone(),
            pane_id: new_pane_id,
        };
        if previous_focus_target.as_ref() != Some(&new_focus_target) {
            self.state.previous_pane_focus = previous_focus_target;
        }
        ws.active_tab_mut()
            .expect("workspace must have an active tab")
            .layout
            .focus_pane(new_pane_id);
        ws.active_tab_mut()
            .expect("workspace must have an active tab")
            .zoomed = true;
        self.overlay_panes.insert(
            new_pane_id,
            super::OverlayPaneState {
                ws_idx,
                tab_idx,
                previous_focus,
                previous_zoomed,
                temp_files,
            },
        );
        self.state.remove_alias_shadowed_by_new_pane(new_pane_id);
        self.state.mode = Mode::Terminal;
        Ok(())
    }

    pub(crate) fn spawn_overlay_argv_command(
        &mut self,
        argv: &[String],
        cwd: Option<std::path::PathBuf>,
        extra_env: Vec<(String, String)>,
        temp_files: Vec<std::path::PathBuf>,
    ) -> std::io::Result<(usize, crate::workspace::NewPane)> {
        let target = self
            .current_plugin_context("overlay-command-pane")
            .focused_pane_id;
        // Overlay pane allocation uses the same public-ID counter as durable panes.
        self.state.prepare_attention_topology_mutation();
        let result = self.spawn_overlay_argv_command_after_attention_canonicalization(
            argv,
            cwd,
            extra_env,
            temp_files,
            target.as_deref(),
        );
        if result.is_err() {
            self.state
                .reconcile_attention_dock_from(&self.terminal_runtimes);
        }
        result
    }

    fn spawn_overlay_argv_command_after_attention_canonicalization(
        &mut self,
        argv: &[String],
        cwd: Option<std::path::PathBuf>,
        extra_env: Vec<(String, String)>,
        temp_files: Vec<std::path::PathBuf>,
        target_pane_id: Option<&str>,
    ) -> std::io::Result<(usize, crate::workspace::NewPane)> {
        let Some((ws_idx, target_pane)) = target_pane_id
            .and_then(|pane_id| self.parse_pane_id(pane_id))
            .or_else(|| {
                let ws_idx = self.state.active?;
                Some((
                    ws_idx,
                    self.state.workspaces.get(ws_idx)?.focused_pane_id()?,
                ))
            })
        else {
            return Err(std::io::Error::other("no active pane"));
        };
        self.state.focus_pane_in_workspace(ws_idx, target_pane);
        let previous_focus_target = self.state.current_pane_focus_target();
        let (rows, cols) = self.state.estimate_pane_size();
        let new_rows = rows.max(4);
        let new_cols = cols.max(10);

        let ws = self
            .state
            .workspaces
            .get(ws_idx)
            .ok_or_else(|| std::io::Error::other("active workspace disappeared"))?;
        let previous_focus = ws
            .focused_pane_id()
            .ok_or_else(|| std::io::Error::other("no focused pane"))?;
        let cwd = cwd.or_else(|| {
            ws.active_tab().and_then(|tab| {
                tab.cwd_for_pane(
                    previous_focus,
                    &self.state.terminals,
                    &self.terminal_runtimes,
                )
            })
        });

        let (tab_idx, new_pane, workspace_id) = {
            let ws = self
                .state
                .workspaces
                .get_mut(ws_idx)
                .ok_or_else(|| std::io::Error::other("active workspace disappeared"))?;
            let previous_zoomed = ws.active_tab().map(|tab| tab.zoomed).unwrap_or(false);
            let result = ws.split_pane_argv_command(
                previous_focus,
                Direction::Horizontal,
                new_rows,
                new_cols,
                cwd,
                argv,
                extra_env,
                self.state.pane_scrollback_limit_bytes,
                self.state.host_terminal_theme,
                self.state.host_terminal_appearance,
                true,
            );
            let (tab_idx, new_pane) = match result {
                Some(Ok(result)) => result,
                Some(Err(err)) => return Err(err),
                None => return Err(std::io::Error::other("focused pane disappeared")),
            };
            ws.tabs
                .get_mut(tab_idx)
                .ok_or_else(|| std::io::Error::other("plugin overlay tab disappeared"))?
                .zoomed = true;
            self.overlay_panes.insert(
                new_pane.pane_id,
                super::OverlayPaneState {
                    ws_idx,
                    tab_idx,
                    previous_focus,
                    previous_zoomed,
                    temp_files,
                },
            );
            (tab_idx, new_pane, ws.id.clone())
        };

        let new_focus_target = crate::app::state::PaneFocusTarget {
            workspace_id,
            pane_id: new_pane.pane_id,
        };
        if previous_focus_target.as_ref() != Some(&new_focus_target) {
            self.state.previous_pane_focus = previous_focus_target;
        }
        self.state.switch_workspace_tab(ws_idx, tab_idx);
        self.state.mode = Mode::Terminal;
        Ok((ws_idx, new_pane))
    }
}

fn write_scrollback_temp_file(content: &str) -> io::Result<std::path::PathBuf> {
    let mut last_collision = None;
    for attempt in 0..16 {
        let path = unique_scrollback_path(attempt);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        match options.open(&path) {
            Ok(mut file) => {
                file.write_all(content.as_bytes())?;
                return Ok(path);
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                last_collision = Some(err);
            }
            Err(err) => return Err(err),
        }
    }

    Err(last_collision.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "failed to create unique scrollback temp file",
        )
    }))
}

fn unique_scrollback_path(attempt: u32) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "herdr-scrollback-{}-{nanos}-{attempt}.txt",
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    fn test_app() -> crate::app::App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    fn binding(action: crate::config::CustomCommandAction) -> crate::config::CustomCommandKeybind {
        crate::config::CustomCommandKeybind {
            bindings: crate::config::ActionKeybinds::prefix("z"),
            label: "prefix+z".into(),
            command: "secret-command --token hidden".into(),
            action,
            description: Some("safe description".into()),
            width: None,
            height: None,
        }
    }

    fn install(app: &mut crate::app::App, binding: crate::config::CustomCommandKeybind) {
        app.endpoint_commands = super::EndpointCommandRegistry::new(&[binding]);
    }

    fn app_with_docked_attention() -> (crate::app::App, crate::layout::PaneId) {
        let mut app = test_app();
        let home = crate::workspace::Workspace::test_new("attention-home");
        let attention_pane = home.tabs[0].root_pane;
        let work = crate::workspace::Workspace::test_new("work");
        app.state.workspaces = vec![home, work];
        app.state.ensure_test_terminals();
        app.state.active = Some(1);
        app.state.selected = 1;
        app.state.mode = crate::app::Mode::Terminal;
        app.state.observe_attention_transition(
            attention_pane,
            crate::detect::AgentState::Working,
            crate::detect::AgentState::Blocked,
            true,
        );
        app.state.make_attention_ready_for_test(attention_pane);
        app.state.reconcile_attention_dock();
        assert!(app.state.workspaces[1].tabs[0]
            .panes
            .contains_key(&attention_pane));
        (app, attention_pane)
    }

    fn assert_unique_live_public_pane_numbers(app: &crate::app::App) {
        for workspace in &app.state.workspaces {
            let live = workspace
                .tabs
                .iter()
                .flat_map(|tab| tab.panes.keys())
                .filter_map(|pane_id| workspace.public_pane_numbers.get(pane_id).copied())
                .collect::<Vec<_>>();
            let unique = live
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>();
            assert_eq!(unique.len(), live.len());
            assert!(workspace.next_public_pane_number > live.into_iter().max().unwrap_or(0));
        }
    }

    #[tokio::test]
    async fn custom_pane_allocation_restores_attention_ids_before_split() {
        let (mut app, attention_pane) = app_with_docked_attention();
        app.state.focus_pane_in_workspace(1, attention_pane);

        app.spawn_pane_command(crate::app::exiting_test_command(), Vec::new())
            .expect("custom pane command");

        assert!(app.state.workspaces[0].tabs[0]
            .panes
            .contains_key(&attention_pane));
        assert!(app
            .overlay_panes
            .values()
            .any(|overlay| overlay.ws_idx == 0));
        assert_unique_live_public_pane_numbers(&app);
        app.state.assert_invariants_for_test();
        crate::app::api::test_support::shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn overlay_allocation_restores_attention_ids_before_split() {
        let (mut app, attention_pane) = app_with_docked_attention();
        app.state.focus_pane_in_workspace(1, attention_pane);
        let argv = vec![crate::app::exiting_test_command().to_owned()];

        let (_, new_pane) = app
            .spawn_overlay_argv_command(&argv, None, Vec::new(), Vec::new())
            .expect("overlay command");
        let terminal_id = new_pane.terminal.id.clone();
        app.terminal_runtimes
            .insert(terminal_id.clone(), new_pane.runtime);
        app.state.terminals.insert(terminal_id, new_pane.terminal);

        assert!(app.state.workspaces[0].tabs[0]
            .panes
            .contains_key(&attention_pane));
        assert_eq!(app.overlay_panes[&new_pane.pane_id].ws_idx, 0);
        assert_unique_live_public_pane_numbers(&app);
        app.state.assert_invariants_for_test();
        crate::app::api::test_support::shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn failed_overlay_command_launch_redocks_ready_attention() {
        let (mut app, attention_pane) = app_with_docked_attention();

        let result = app.spawn_overlay_argv_command(&[], None, Vec::new(), Vec::new());

        assert!(result.is_err());
        assert_eq!(app.state.docked_attention_pane(), Some(attention_pane));
        assert!(app.state.workspaces[1].tabs[0]
            .panes
            .contains_key(&attention_pane));
        app.state.assert_invariants_for_test();
        crate::app::api::test_support::shutdown_test_runtimes(&mut app);
    }

    #[test]
    fn manifest_exposes_opaque_ids_without_command_text() {
        let mut app = test_app();
        install(&mut app, binding(crate::config::CustomCommandAction::Shell));
        let manifest = app.client_shell_command_manifest();
        assert_eq!(manifest.len(), 1);
        assert_eq!(manifest[0].binding_label, "prefix+z");
        assert_eq!(manifest[0].binding_labels, ["prefix+z"]);
        assert_eq!(manifest[0].description.as_deref(), Some("safe description"));
        assert!(!format!("{:?}", manifest).contains("secret-command"));
        assert_eq!(
            app.resolve_client_shell_command(&manifest[0].command_id)
                .map(|binding| binding.command),
            Some("secret-command --token hidden".into())
        );
    }

    #[test]
    fn stale_command_id_is_rejected_after_definition_changes() {
        let mut app = test_app();
        install(&mut app, binding(crate::config::CustomCommandAction::Shell));
        let old_id = app.client_shell_command_manifest()[0].command_id.clone();
        let mut replacement = binding(crate::config::CustomCommandAction::Shell);
        replacement.command = "replacement-command".into();
        install(&mut app, replacement);

        let response = app.handle_command_invoke(
            "request-1".into(),
            crate::api::schema::CommandInvokeParams {
                command_id: old_id,
                workspace_id: None,
                tab_id: None,
                pane_id: None,
                selection: None,
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "command_not_found");
    }

    #[test]
    fn foreground_keybinding_projection_cannot_remap_command_ids() {
        let mut app = test_app();
        install(&mut app, binding(crate::config::CustomCommandAction::Shell));
        let command_id = app.client_shell_command_manifest()[0].command_id.clone();
        let mut foreground_binding = binding(crate::config::CustomCommandAction::Shell);
        foreground_binding.command = "different-foreground-command".into();
        app.state.keybinds.custom_commands = vec![foreground_binding];

        assert_eq!(
            app.resolve_client_shell_command(&command_id)
                .map(|binding| binding.command),
            Some("secret-command --token hidden".into())
        );
    }

    #[test]
    fn command_ids_do_not_alias_across_endpoint_restarts() {
        let mut old_app = test_app();
        install(
            &mut old_app,
            binding(crate::config::CustomCommandAction::Shell),
        );
        let old_id = old_app.client_shell_command_manifest()[0]
            .command_id
            .clone();

        let mut replacement_app = test_app();
        let mut replacement = binding(crate::config::CustomCommandAction::Shell);
        replacement.command = "replacement-command".into();
        install(&mut replacement_app, replacement);
        assert_ne!(
            replacement_app.client_shell_command_manifest()[0].command_id,
            old_id
        );
        let response = replacement_app.handle_command_invoke(
            "request-1".into(),
            crate::api::schema::CommandInvokeParams {
                command_id: old_id,
                workspace_id: None,
                tab_id: None,
                pane_id: None,
                selection: None,
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "command_not_found");
    }

    #[tokio::test]
    async fn plugin_command_rejects_stale_client_selection_before_invocation() {
        let mut app = test_app();
        let workspace = crate::workspace::Workspace::test_new("plugin-selection");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.terminal_runtimes.insert(
            terminal_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"selected text\n"),
        );
        let mut plugin = binding(crate::config::CustomCommandAction::PluginAction);
        plugin.command = "missing.plugin-action".into();
        install(&mut app, plugin);
        let command_id = app.client_shell_command_manifest()[0].command_id.clone();
        let workspace_id = app.public_workspace_id(0);
        let tab_id = app.public_tab_id(0, 0).unwrap();
        let pane_id = app.public_pane_id(0, pane_id).unwrap();

        let response = app.handle_command_invoke(
            "request-selection".into(),
            crate::api::schema::CommandInvokeParams {
                command_id,
                workspace_id: Some(workspace_id),
                tab_id: Some(tab_id),
                pane_id: Some(pane_id.clone()),
                selection: Some(crate::api::schema::PaneSelectionReadParams {
                    pane_id,
                    anchor: crate::api::schema::PaneTextPoint { row: 0, col: 0 },
                    cursor: crate::api::schema::PaneTextPoint { row: 0, col: 7 },
                    content_revision: Some(u64::MAX),
                }),
            },
        );

        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "stale_content");
    }

    #[cfg(unix)]
    #[test]
    fn shell_command_invocation_executes_endpoint_owned_definition() {
        let mut app = test_app();
        let path = std::path::PathBuf::from(format!(
            "/var/tmp/herdr-command-invoke-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        let mut command = binding(crate::config::CustomCommandAction::Shell);
        command.command = format!("printf invoked > {}", path.display());
        install(&mut app, command);
        let command_id = app.client_shell_command_manifest()[0].command_id.clone();

        let response = app.handle_command_invoke(
            "request-1".into(),
            crate::api::schema::CommandInvokeParams {
                command_id,
                workspace_id: None,
                tab_id: None,
                pane_id: None,
                selection: None,
            },
        );
        let success: crate::api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.result, crate::api::schema::ResponseResult::Ok {});
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "invoked");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn popup_commands_are_advertised_without_exposing_the_command_text() {
        let mut app = test_app();
        install(&mut app, binding(crate::config::CustomCommandAction::Popup));

        let manifest = app.client_shell_command_manifest();
        assert_eq!(manifest.len(), 1);
        assert_eq!(
            manifest[0].action,
            crate::protocol::ClientShellCommandAction::Popup
        );
        assert!(!format!("{manifest:?}").contains("secret-command"));
    }
}
