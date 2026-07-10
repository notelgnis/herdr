use crate::app;
use crate::app::state::AppState;
use crate::config;
use crate::detect::AgentState;
use crate::layout::PaneId;
use crate::protocol;
use crate::terminal::{TerminalId, TerminalRuntimeRegistry};

pub(crate) fn should_forward_toast_to_clients(delivery: config::ToastDelivery) -> bool {
    toast_notify_kind(delivery).is_some()
}

pub(crate) fn toast_notify_kind(delivery: config::ToastDelivery) -> Option<protocol::NotifyKind> {
    match delivery {
        config::ToastDelivery::Terminal => Some(protocol::NotifyKind::Toast),
        config::ToastDelivery::System => Some(protocol::NotifyKind::SystemToast),
        config::ToastDelivery::Off | config::ToastDelivery::Herdr => None,
    }
}

pub(crate) fn toast_message_from_state_change(
    state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    pane_id: PaneId,
    suppress_active_tab_notifications: bool,
    prev_state: AgentState,
    new_state: AgentState,
    previous_agent_label: Option<&str>,
) -> Option<String> {
    state.workspaces.iter().find_map(|ws| {
        ws.tabs.iter().find_map(|tab| {
            let pane = tab.panes.get(&pane_id)?;
            let agent_label = state
                .terminals
                .get(&pane.attached_terminal_id)
                .and_then(|terminal| terminal.effective_agent_label())?;
            let kind = app::actions::notification_toast_for_state_change_with_agent_labels(
                suppress_active_tab_notifications,
                prev_state,
                new_state,
                previous_agent_label,
                Some(agent_label),
            )?;
            let workspace_label = ws.display_name_from(&state.terminals, terminal_runtimes);
            let context = forwarded_notification_context(
                workspace_label,
                terminal_runtimes,
                &pane.attached_terminal_id,
            );
            Some(format!(
                "{} {}: {}",
                agent_label,
                toast_event_text(kind),
                context
            ))
        })
    })
}

/// Body for a forwarded desktop notification (Terminal/System delivery).
///
/// Favors the agent's own terminal title (e.g. Claude Code sets it to a summary
/// of the current task) and falls back to the workspace label. Unlike the
/// in-app toast, it omits the sidebar ordinal, which is meaningless in a desktop
/// notification. This is used for every forwarded path (immediate and delayed);
/// the numbered [`app::actions::notification_context`] is reserved for the
/// in-app Herdr toast rendered in the sidebar.
pub(crate) fn forwarded_notification_context(
    workspace_label: String,
    terminal_runtimes: &TerminalRuntimeRegistry,
    terminal_id: &TerminalId,
) -> String {
    let osc_title = terminal_runtimes
        .get(terminal_id)
        .map(|runtime| runtime.agent_osc_title())
        .unwrap_or_default();
    clean_agent_osc_title(&osc_title, &workspace_label).unwrap_or(workspace_label)
}

/// Turn an agent's raw OSC 0/2 terminal title into a notification-worthy task
/// label, or `None` when the title carries no useful task information.
///
/// Agents prepend a status marker to the title (a `✳` idle glyph or a braille
/// spinner frame while working); that glyph is stripped. Titles that are empty,
/// the bare agent name, or just the project label (Codex sets the title to the
/// folder name) are rejected so the caller falls back to the project label.
fn clean_agent_osc_title(raw: &str, project: &str) -> Option<String> {
    let mut title = raw.trim();
    if let Some(first) = title.chars().next() {
        let is_status_glyph = first == '✳' || ('\u{2800}'..='\u{28FF}').contains(&first);
        if is_status_glyph {
            title = title[first.len_utf8()..].trim_start();
        }
    }
    let title = title.trim();
    if title.is_empty()
        || title.eq_ignore_ascii_case(project)
        || title.eq_ignore_ascii_case("claude code")
        || title.eq_ignore_ascii_case("codex")
    {
        return None;
    }
    Some(title.to_owned())
}

fn toast_event_text(kind: app::state::ToastKind) -> &'static str {
    match kind {
        app::state::ToastKind::NeedsAttention => "needs attention",
        app::state::ToastKind::Finished => "finished",
        app::state::ToastKind::UpdateInstalled => "updated",
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::*;
    #[cfg(unix)]
    use crate::detect::Agent;
    #[cfg(unix)]
    use crate::terminal::TerminalState;

    #[cfg(unix)]
    fn init_repo(path: &std::path::Path) {
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success(), "git init failed for {}", path.display());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn toast_message_uses_live_root_runtime_cwd_label() {
        let mut state = AppState::test_new();
        state
            .workspaces
            .push(crate::workspace::Workspace::test_new("stale"));
        state.ensure_test_terminals();
        let root = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0].terminal_id(root).cloned().unwrap();
        let temp_root = std::env::temp_dir().join(format!(
            "herdr-forwarded-toast-context-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let stale_cwd = temp_root.join("__herdr_original__");
        let live_cwd = temp_root.join("__herdr_projects__");
        std::fs::create_dir_all(&stale_cwd).unwrap();
        std::fs::create_dir_all(&live_cwd).unwrap();
        init_repo(&stale_cwd);
        init_repo(&live_cwd);
        state.workspaces[0].custom_name = None;
        state.workspaces[0].identity_cwd = stale_cwd.clone();
        let mut terminal = TerminalState::new(terminal_id.clone(), stale_cwd);
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        state.terminals.insert(terminal_id.clone(), terminal);
        let (events, _) = tokio::sync::mpsc::channel(4);
        let runtime = crate::terminal::TerminalRuntime::spawn(
            root,
            24,
            80,
            live_cwd.clone(),
            0,
            crate::terminal_theme::TerminalTheme::default(),
            crate::pane::PaneShellConfig::new("/bin/sh", crate::config::ShellModeConfig::NonLogin),
            &crate::pane::PaneLaunchEnv::default(),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd() != Some(live_cwd.clone()) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        terminal_runtimes.insert(terminal_id, runtime);

        let message = toast_message_from_state_change(
            &state,
            &terminal_runtimes,
            root,
            false,
            AgentState::Working,
            AgentState::Idle,
            Some("codex"),
        );

        // No OSC title is set on the runtime, so the body falls back to the live
        // workspace label with no sidebar ordinal.
        assert_eq!(
            message.as_deref(),
            Some("codex finished: __herdr_projects__")
        );

        for (_, runtime) in terminal_runtimes.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(temp_root);
    }

    #[test]
    fn clean_agent_osc_title_strips_status_glyphs() {
        assert_eq!(
            super::clean_agent_osc_title("✳ Explain design skills capabilities", "ScoreFlow"),
            Some("Explain design skills capabilities".to_owned())
        );
        assert_eq!(
            super::clean_agent_osc_title("⠐ Customize Herdr UI like Zellij", "herdr"),
            Some("Customize Herdr UI like Zellij".to_owned())
        );
    }

    #[test]
    fn clean_agent_osc_title_rejects_uninformative_titles() {
        // Fresh Claude Code panes advertise the bare agent name.
        assert_eq!(super::clean_agent_osc_title("✳ Claude Code", "herdr"), None);
        // Codex sets the title to the project folder, which is redundant.
        assert_eq!(super::clean_agent_osc_title("herdr", "herdr"), None);
        assert_eq!(super::clean_agent_osc_title("   ", "herdr"), None);
    }
}
