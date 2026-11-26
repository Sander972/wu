use agent_harness::{ClaudeLaunch, PermissionMode, ReasoningLevel};
use settings::{
    AgentChatAgent, AgentChatDefaultEffort, AgentChatSpinnerVerbsMode, AgentChatWhileWorking,
    AntigravityPermissionDefault, ClaudeCodePermissionDefault, CodexPermissionDefault,
    ContextTokens, DockSide, OpencodePermissionDefault, RegisterSetting, Settings,
};
pub use settings::{AgentChatModelPickerLayout, AgentChatSendKey};

use crate::AgentKind;

#[derive(Clone, Debug, RegisterSetting)]
pub struct AgentChatSettings {
    pub enabled: bool,
    pub dock: DockSide,
    pub chat_panel_dock: DockSide,
    pub send_with: AgentChatSendKey,
    pub compact_transcript: bool,
    pub model_picker: AgentChatModelPickerLayout,
    pub dictation: bool,
    pub move_to_panel_button: bool,
    pub new_chat_from_tab_bar: bool,
    pub saved_messages_button: bool,
    pub open_in_terminal_button: bool,
    pub sound_when_done: bool,
    pub sound_when_needs_input: bool,
    pub sound_on_error: bool,
    pub notifications: bool,
    pub notify_only_in_background: bool,
    pub auto_compact_tokens: Option<u64>,
    pub default_effort: Option<ReasoningLevel>,
    default_permissions: [(AgentKind, PermissionMode); 4],
    agents: Vec<AgentKind>,
    pub steer_while_working: bool,
    pub confirm_quit_while_working: bool,
    pub continue_after_reload: bool,
    pub prevent_idle_sleep: bool,
    pub claude_in_chrome: bool,
    pub prompt_suggestions: bool,
    pub autosave: bool,
    pub attach_selection: bool,
    pub attach_open_file: bool,
    pub show_fix_actions: bool,
    pub enable_new_conversation_shortcut: bool,
    pub enable_reopen_closed_session_shortcut: bool,
    pub archive_inactive_days: u32,
    pub escape_stops: bool,
    pub scroll_to_bottom_on_send: bool,
    pub show_message_timestamps: bool,
    pub context_usage_threshold: u32,
    pub allow_bypass_permissions: bool,
    pub claude_launch: ClaudeLaunch,
    pub commit_message: CommitMessageSettings,
}

#[derive(Clone, Debug, Default)]
pub struct CommitMessageSettings {
    pub agent: Option<AgentKind>,
    pub model: Option<String>,
    pub instructions: Option<String>,
}

impl AgentChatSettings {
    pub fn default_permission(&self, kind: AgentKind) -> PermissionMode {
        let mode = self
            .default_permissions
            .iter()
            .find(|(agent, _)| *agent == kind)
            .map_or(PermissionMode::Auto, |(_, mode)| *mode);
        self.allowed_permission(kind, mode)
    }

    /// Bypass is only Claude Code's to hide: the other agents' full access is not what `allow_bypass_permissions` is about.
    pub fn allowed_permission(&self, kind: AgentKind, mode: PermissionMode) -> PermissionMode {
        if kind == AgentKind::Claude
            && mode == PermissionMode::FullAccess
            && !self.allow_bypass_permissions
        {
            PermissionMode::Auto
        } else {
            mode
        }
    }

    pub fn permission_choices(&self, kind: AgentKind) -> Vec<PermissionMode> {
        PermissionMode::choices(kind.harness_id())
            .iter()
            .copied()
            .filter(|mode| self.allowed_permission(kind, *mode) == *mode)
            .collect()
    }

    pub fn agents(&self) -> &[AgentKind] {
        &self.agents
    }
}

impl Settings for AgentChatSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let content = content.agent_chat.clone().unwrap_or_default();
        let permission = content.default_permission.clone().unwrap_or_default();
        let shown = content.agents.clone().unwrap_or_default();
        let mut agents: Vec<AgentKind> = [
            (AgentKind::Claude, shown.claude_code),
            (AgentKind::Codex, shown.codex),
            (AgentKind::Opencode, shown.opencode),
            (AgentKind::Antigravity, shown.antigravity),
        ]
        .into_iter()
        .filter(|(_, shown)| shown.unwrap_or(true))
        .map(|(kind, _)| kind)
        .collect();
        if agents.is_empty() {
            agents = AgentKind::ALL.to_vec();
        }
        Self {
            enabled: content.enabled.unwrap_or(true),
            dock: content.dock.unwrap_or(DockSide::Left),
            chat_panel_dock: content.chat_panel_dock.unwrap_or(DockSide::Right),
            send_with: content.send_with.unwrap_or_default(),
            compact_transcript: content.compact_transcript.unwrap_or(false),
            model_picker: content.model_picker.unwrap_or_default(),
            dictation: content.dictation.unwrap_or(true),
            move_to_panel_button: content.move_to_panel_button.unwrap_or(false),
            new_chat_from_tab_bar: content.new_chat_from_tab_bar.unwrap_or(true),
            saved_messages_button: content.saved_messages_button.unwrap_or(true),
            open_in_terminal_button: content.open_in_terminal_button.unwrap_or(true),
            sound_when_done: content.sound_when_done.unwrap_or(true),
            sound_when_needs_input: content.sound_when_needs_input.unwrap_or(true),
            sound_on_error: content.sound_on_error.unwrap_or(true),
            notifications: content.notifications.unwrap_or(false),
            notify_only_in_background: content.notify_only_in_background.unwrap_or(true),
            auto_compact_tokens: content.auto_compact.unwrap_or(false).then(|| {
                content
                    .auto_compact_limit
                    .unwrap_or(ContextTokens(200_000))
                    .clamped()
            }),
            default_effort: effort(content.default_effort.unwrap_or_default()),
            default_permissions: [
                (
                    AgentKind::Claude,
                    claude_permission(permission.claude_code.unwrap_or_default()),
                ),
                (
                    AgentKind::Codex,
                    codex_permission(permission.codex.unwrap_or_default()),
                ),
                (
                    AgentKind::Opencode,
                    opencode_permission(permission.opencode.unwrap_or_default()),
                ),
                (
                    AgentKind::Antigravity,
                    antigravity_permission(permission.antigravity.unwrap_or_default()),
                ),
            ],
            agents,
            steer_while_working: content.while_working.unwrap_or_default()
                == AgentChatWhileWorking::Steer,
            confirm_quit_while_working: content.confirm_quit_while_working.unwrap_or(true),
            continue_after_reload: content.continue_after_reload.unwrap_or(true),
            prevent_idle_sleep: content.prevent_idle_sleep.unwrap_or(true),
            claude_in_chrome: content.claude_in_chrome.unwrap_or(true),
            prompt_suggestions: content.prompt_suggestions.unwrap_or(true),
            autosave: content.autosave.unwrap_or(true),
            attach_selection: content.attach_selection.unwrap_or(true),
            attach_open_file: content.attach_open_file.unwrap_or(true),
            show_fix_actions: content.show_fix_actions.unwrap_or(true),
            enable_new_conversation_shortcut: content
                .enable_new_conversation_shortcut
                .unwrap_or(false),
            enable_reopen_closed_session_shortcut: content
                .enable_reopen_closed_session_shortcut
                .unwrap_or(true),
            archive_inactive_days: content.archive_inactive_sessions.unwrap_or_default().days(),
            escape_stops: content.escape_stops.unwrap_or(true),
            scroll_to_bottom_on_send: content.scroll_to_bottom_on_send.unwrap_or(true),
            show_message_timestamps: content.show_message_timestamps.unwrap_or(true),
            context_usage_threshold: content.context_usage_threshold.unwrap_or(50).min(100),
            allow_bypass_permissions: content.allow_bypass_permissions.unwrap_or(true),
            claude_launch: {
                let spinner_verbs = content.spinner_verbs.unwrap_or_default();
                ClaudeLaunch {
                    environment: content
                        .environment_variables
                        .unwrap_or_default()
                        .into_iter()
                        .map(|variable| (variable.name, variable.value))
                        .collect(),
                    process_wrapper: content
                        .process_wrapper
                        .filter(|wrapper| !wrapper.trim().is_empty()),
                    spinner_verbs: spinner_verbs.verbs.unwrap_or_default(),
                    replace_spinner_verbs: spinner_verbs.mode
                        == Some(AgentChatSpinnerVerbsMode::Replace),
                }
            },
            commit_message: {
                let commit_message = content.commit_message.unwrap_or_default();
                CommitMessageSettings {
                    agent: commit_message.agent.and_then(AgentKind::from_setting),
                    model: commit_message
                        .model
                        .filter(|model| !model.trim().is_empty()),
                    instructions: commit_message
                        .instructions
                        .filter(|instructions| !instructions.trim().is_empty()),
                }
            },
        }
    }
}

fn effort(effort: AgentChatDefaultEffort) -> Option<ReasoningLevel> {
    match effort {
        AgentChatDefaultEffort::LastUsed => None,
        AgentChatDefaultEffort::Minimal => Some(ReasoningLevel::Minimal),
        AgentChatDefaultEffort::Low => Some(ReasoningLevel::Low),
        AgentChatDefaultEffort::Medium => Some(ReasoningLevel::Medium),
        AgentChatDefaultEffort::High => Some(ReasoningLevel::High),
        AgentChatDefaultEffort::XHigh => Some(ReasoningLevel::XHigh),
        AgentChatDefaultEffort::Max => Some(ReasoningLevel::Max),
    }
}

fn claude_permission(mode: ClaudeCodePermissionDefault) -> PermissionMode {
    match mode {
        ClaudeCodePermissionDefault::Ask => PermissionMode::Ask,
        ClaudeCodePermissionDefault::Plan => PermissionMode::Plan,
        ClaudeCodePermissionDefault::AcceptEdits => PermissionMode::AcceptEdits,
        ClaudeCodePermissionDefault::Auto => PermissionMode::Auto,
        ClaudeCodePermissionDefault::DontAsk => PermissionMode::DontAsk,
        ClaudeCodePermissionDefault::Bypass => PermissionMode::FullAccess,
    }
}

fn codex_permission(mode: CodexPermissionDefault) -> PermissionMode {
    match mode {
        CodexPermissionDefault::ReadOnly => PermissionMode::ReadOnly,
        CodexPermissionDefault::Auto => PermissionMode::Auto,
        CodexPermissionDefault::ApproveForMe => PermissionMode::ApproveForMe,
        CodexPermissionDefault::FullAccess => PermissionMode::FullAccess,
    }
}

fn opencode_permission(mode: OpencodePermissionDefault) -> PermissionMode {
    match mode {
        OpencodePermissionDefault::Ask => PermissionMode::Ask,
        OpencodePermissionDefault::AcceptEdits => PermissionMode::AcceptEdits,
        OpencodePermissionDefault::Auto => PermissionMode::Auto,
        OpencodePermissionDefault::FullAccess => PermissionMode::FullAccess,
    }
}

fn antigravity_permission(mode: AntigravityPermissionDefault) -> PermissionMode {
    match mode {
        AntigravityPermissionDefault::Ask => PermissionMode::Ask,
        AntigravityPermissionDefault::AcceptEdits => PermissionMode::AcceptEdits,
        AntigravityPermissionDefault::Auto => PermissionMode::Auto,
        AntigravityPermissionDefault::FullAccess => PermissionMode::FullAccess,
    }
}

impl AgentKind {
    pub(crate) fn from_setting(agent: AgentChatAgent) -> Option<Self> {
        match agent {
            AgentChatAgent::Automatic => None,
            AgentChatAgent::ClaudeCode => Some(AgentKind::Claude),
            AgentChatAgent::Codex => Some(AgentKind::Codex),
            AgentChatAgent::Opencode => Some(AgentKind::Opencode),
            AgentChatAgent::Antigravity => Some(AgentKind::Antigravity),
        }
    }

    pub(crate) fn to_setting(self) -> AgentChatAgent {
        match self {
            AgentKind::Claude => AgentChatAgent::ClaudeCode,
            AgentKind::Codex => AgentChatAgent::Codex,
            AgentKind::Opencode => AgentChatAgent::Opencode,
            AgentKind::Antigravity => AgentChatAgent::Antigravity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_with(
        update: impl FnOnce(&mut settings::AgentChatSettingsContent),
    ) -> AgentChatSettings {
        let mut content = settings::SettingsContent::default();
        update(content.agent_chat.get_or_insert_default());
        AgentChatSettings::from_settings(&content)
    }

    #[test]
    fn bypass_is_hidden_when_not_allowed() {
        let bypass_by_default = |allowed: Option<bool>| {
            settings_with(|content| {
                content.allow_bypass_permissions = allowed;
                content
                    .default_permission
                    .get_or_insert_default()
                    .claude_code = Some(ClaudeCodePermissionDefault::Bypass);
                content.default_permission.get_or_insert_default().codex =
                    Some(CodexPermissionDefault::FullAccess);
            })
        };

        let allowed = bypass_by_default(None);
        assert_eq!(
            allowed.default_permission(AgentKind::Claude),
            PermissionMode::FullAccess
        );
        assert!(
            allowed
                .permission_choices(AgentKind::Claude)
                .contains(&PermissionMode::FullAccess)
        );

        let denied = bypass_by_default(Some(false));
        assert_eq!(
            denied.default_permission(AgentKind::Claude),
            PermissionMode::Auto,
            "a configured Bypass default falls back to Auto"
        );
        assert_eq!(
            denied.allowed_permission(AgentKind::Claude, PermissionMode::FullAccess),
            PermissionMode::Auto,
            "a chat that stored Bypass falls back too"
        );
        let choices = denied.permission_choices(AgentKind::Claude);
        assert!(!choices.contains(&PermissionMode::FullAccess));
        assert_eq!(
            choices.len(),
            PermissionMode::choices(agent_harness::HarnessId::ClaudeCode).len() - 1
        );

        assert_eq!(
            denied.default_permission(AgentKind::Codex),
            PermissionMode::FullAccess,
            "other agents keep their full access"
        );
        assert!(
            denied
                .permission_choices(AgentKind::Codex)
                .contains(&PermissionMode::FullAccess)
        );
    }

    #[test]
    fn launch_settings_reach_the_claude_launch() {
        let configured = settings_with(|content| {
            content.process_wrapper = Some("  ".into());
            content.environment_variables = Some(vec![settings::AgentChatEnvironmentVariable {
                name: "HTTPS_PROXY".into(),
                value: "http://localhost:8080".into(),
            }]);
            content.spinner_verbs = Some(settings::AgentChatSpinnerVerbsContent {
                mode: Some(settings::AgentChatSpinnerVerbsMode::Replace),
                verbs: Some(vec!["Pondering".into()]),
            });
        });
        assert_eq!(
            configured.claude_launch,
            ClaudeLaunch {
                environment: vec![("HTTPS_PROXY".into(), "http://localhost:8080".into())],
                process_wrapper: None,
                spinner_verbs: vec!["Pondering".into()],
                replace_spinner_verbs: true,
            }
        );
        assert_eq!(settings_with(|_| {}).claude_launch, ClaudeLaunch::default());
    }
}
