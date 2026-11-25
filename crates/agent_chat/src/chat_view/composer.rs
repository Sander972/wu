use super::*;
use crate::{
    AgentKind,
    chat_style::{composer_surface, icon, icon_size_px, tracked_upper, working_animation},
    session::AgentUpdateStatus,
};
use agent_harness::{PermissionDecision, PermissionMode, PermissionPrompt};
use i18n::t;
use workspace::{OpenOptions, OpenVisible, Toast, notifications::NotificationId};

const PERMISSION_DESTINATIONS: [&str; 4] = [
    "localSettings",
    "userSettings",
    "projectSettings",
    "session",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum PermissionChoice {
    Allow,
    AlwaysAllow,
    Deny,
}

fn destination_label(destination: &str) -> String {
    match destination {
        "localSettings" => t!("this project (just you)"),
        "userSettings" => t!("all projects"),
        "projectSettings" => t!("this project (shared)"),
        _ => t!("this session"),
    }
    .to_string()
}

pub(super) fn bash_command(prompt: &PermissionPrompt) -> Option<&str> {
    if prompt.tool_name != "Bash" {
        return None;
    }
    prompt.input.get("command")?.as_str()
}

pub(super) fn plan_text(prompt: &PermissionPrompt) -> Option<&str> {
    if prompt.tool_name != "ExitPlanMode" {
        return None;
    }
    prompt.input.get("plan")?.as_str()
}

pub(super) fn suggested_destination(suggestions: &[serde_json::Value]) -> String {
    suggestions
        .iter()
        .find_map(|suggestion| suggestion.get("destination")?.as_str())
        .unwrap_or("session")
        .to_string()
}

/// Mirrors the official extension: a mode change is dropped when rules are suggested, and only non-mode suggestions follow the chosen destination.
pub(crate) fn always_allow_updates(
    suggestions: &[serde_json::Value],
    destination: &str,
) -> Vec<serde_json::Value> {
    let suggests_rules = suggestions.iter().any(|suggestion| {
        suggestion["type"] == "addRules"
            && suggestion["rules"]
                .as_array()
                .is_some_and(|rules| !rules.is_empty())
    });
    suggestions
        .iter()
        .filter(|suggestion| !suggests_rules || suggestion["type"] != "setMode")
        .map(|suggestion| {
            let mut suggestion = suggestion.clone();
            if suggestion["type"] != "setMode"
                && let Some(object) = suggestion.as_object_mut()
            {
                object.insert("destination".into(), destination.into());
            }
            suggestion
        })
        .collect()
}

/// `prompt` is None for harnesses that only read the Allow or Deny label (Codex, ACP, Opencode).
fn permission_decision(
    prompt: Option<&PermissionPrompt>,
    choice: PermissionChoice,
    destination: &str,
    command: Option<&str>,
    reason: &str,
) -> PermissionDecision {
    if choice == PermissionChoice::Deny {
        let reason = reason.trim();
        return if reason.is_empty() {
            PermissionDecision::Deny {
                message: agent_harness::claude::DENY_STOP.into(),
                interrupt: true,
            }
        } else {
            PermissionDecision::Deny {
                message: format!("{}{reason}", agent_harness::claude::DENY_REASON_PREFIX),
                interrupt: false,
            }
        };
    }
    let updated_input = command
        .map(str::trim)
        .filter(|command| {
            !command.is_empty() && Some(*command) != prompt.and_then(bash_command).map(str::trim)
        })
        .and_then(|command| {
            let mut input = prompt?.input.clone();
            input
                .as_object_mut()?
                .insert("command".into(), command.into());
            Some(input)
        });
    let updated_permissions = if choice == PermissionChoice::AlwaysAllow {
        always_allow_updates(prompt.map_or(&[], |prompt| &prompt.suggestions), destination)
    } else {
        Vec::new()
    };
    PermissionDecision::Allow {
        updated_input,
        updated_permissions,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlanComment {
    pub quote: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PlanChoice {
    AcceptEdits,
    Auto,
    Manual,
    KeepPlanning {
        feedback: String,
        comments: Vec<PlanComment>,
    },
}

/// The mode, when present, is switched by wu before answering; manual approval instead lets the CLI leave plan mode through `setMode`.
pub(crate) fn plan_decision(choice: &PlanChoice) -> (Option<PermissionMode>, PermissionDecision) {
    let allow = |updated_permissions| PermissionDecision::Allow {
        updated_input: None,
        updated_permissions,
    };
    match choice {
        PlanChoice::AcceptEdits => (Some(PermissionMode::AcceptEdits), allow(Vec::new())),
        PlanChoice::Auto => (Some(PermissionMode::Auto), allow(Vec::new())),
        PlanChoice::Manual => (
            None,
            allow(vec![serde_json::json!({
                "type": "setMode",
                "mode": "default",
                "destination": "session",
            })]),
        ),
        PlanChoice::KeepPlanning { feedback, comments } => {
            let feedback = feedback.trim();
            let mut message = if feedback.is_empty() {
                agent_harness::claude::KEEP_PLANNING.to_string()
            } else {
                format!("{}{feedback}", agent_harness::claude::DENY_REASON_PREFIX)
            };
            if !comments.is_empty() {
                message.push_str("\n\nComments on the plan:");
                for comment in comments {
                    message.push_str(&format!("\n[Re: \"{}\"] {}", comment.quote, comment.text));
                }
            }
            let interrupt = feedback.is_empty() && comments.is_empty();
            (None, PermissionDecision::Deny { message, interrupt })
        }
    }
}

const NEW_CHAT_TEXTAREA_MIN: f32 = 76.;
const TEXTAREA_MAX: f32 = 260.;
const ACTIONS_ROW_HEIGHT: f32 = 42.;
const ACTION_BUTTON_SIZE: f32 = 28.;
const ACTION_PRIMARY_GAP: f32 = 8.;
const MODEL_CHIP_PADDING: f32 = 6.;
const ACTION_INSET: f32 = 12.;
const MODEL_CHIP_MAX_FRACTION: f32 = 0.45;
const TRAY_RADIUS: f32 = 16.;
pub(super) const TRAY_SIDE_INSET: f32 = 16.;
pub(super) const TRAY_COMPOSER_OVERLAP: f32 = 18.;
const QUESTION_LINE_HEIGHT: f32 = 20.;
const QUESTION_COLLAPSED_LINES: usize = 4;
const QUESTION_COLLAPSE_CHARS: usize = 160;
const QUESTION_EXPANDED_FRACTION: f32 = 0.4;
const QUESTION_EXPANDED_FALLBACK: f32 = 280.;
const QUESTION_OPTIONS_FRACTION: f32 = 0.3;
const PLAN_QUOTE_CHARS: usize = 80;

pub(super) fn tray_surface(cx: &App) -> gpui::Div {
    div()
        .rounded_t(px(TRAY_RADIUS))
        .bg(composer_surface(cx))
        .border_1()
        .border_color(cx.theme().colors().border)
        .shadow_lg()
        .px(px(4.))
        .pt(px(4.))
        .pb(px(TRAY_COMPOSER_OVERLAP))
        .flex()
        .flex_col()
}

impl ChatView {
    pub(super) fn render_model_chip(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        let kind = session.kind();
        let settings = session.settings();
        let selection = Selection::resolve(self.store.read(cx).models(kind), settings);
        let model_label = selection.model_label(settings, kind);
        let effort = selection.reasoning.map(view::reasoning_label);
        let fast = selection.fast_on();
        let colors = cx.theme().colors();
        let brand = match kind {
            AgentKind::Codex | AgentKind::Opencode | AgentKind::Antigravity => {
                agent_icon(kind).color(Color::Muted)
            }
            AgentKind::Claude => agent_icon(kind),
        }
        .size(icon_size_px(16., window));
        let session = self.session.clone();
        let store = self.store.clone();
        PopoverMenu::new(SharedString::from(format!(
            "agent-model-picker-{}",
            cx.entity_id()
        )))
        .trigger(
            Chip::new(
                "agent-model-chip",
                16.,
                h_flex()
                    .h(px(32.))
                    .max_w(px(248.))
                    .min_w_0()
                    .px(px(MODEL_CHIP_PADDING))
                    .gap(px(6.))
                    .text_size(ui(12.))
                    .font_weight(FontWeight::NORMAL)
                    .child(brand)
                    .child(div().min_w_0().truncate().child(model_label))
                    .when_some(effort, |this, effort| {
                        this.child(
                            div()
                                .map(|mut suffix| {
                                    suffix.style().flex_shrink = Some(1000.);
                                    suffix
                                })
                                .min_w_0()
                                .truncate()
                                .text_color(colors.text_muted.opacity(0.7))
                                .child(effort),
                        )
                    })
                    .when(fast, |this| {
                        this.child(icon(IconName::AgentFastFilled, px(13.), accent(cx)))
                    })
                    .child(icon(IconName::AgentArrowDown, px(12.), colors.text_muted)),
            )
            .shrinkable()
            .text_colors(colors.text.opacity(0.9), colors.text),
        )
        .menu(move |window, cx| {
            let session = session.clone();
            let store = store.clone();
            Some(cx.new(|cx| ModelPicker::new(session, store, window, cx)))
        })
        .with_handle(self.model_picker.clone())
        .anchor(Anchor::BottomRight)
        .attach(Anchor::TopRight)
        .offset(point(px(0.), px(-6.)))
    }

    pub(super) fn render_send_button(&self, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let is_working = self.session.read(cx).is_working();
        let has_text = self.staging_count == 0
            && (!self.composer.read(cx).text(cx).trim().is_empty()
                || !self.attachments.is_empty());
        let focus_handle = self.composer.focus_handle(cx);
        let plate = page(cx);
        let circle = |id: &'static str| {
            div()
                .id(id)
                .size(px(SEND_BUTTON_SIZE))
                .flex_none()
                .rounded_full()
                .bg(colors.text)
                .flex()
                .items_center()
                .justify_center()
        };
        if is_working && !has_text {
            return circle("agent-stop")
                .cursor_pointer()
                .hover(|style| style.opacity(0.85))
                .tooltip(move |_, cx| Tooltip::for_action_in(t!("Stop"), &Stop, &focus_handle, cx))
                .on_click(cx.listener(|this, _, window, cx| this.stop(&Stop, window, cx)))
                .child(div().size(px(11.)).rounded(px(3.)).bg(plate))
                .into_any_element();
        }
        circle("agent-send")
            .when(!has_text, |this| this.opacity(0.35))
            .when(has_text, |this| {
                this.cursor_pointer()
                    .hover(|style| style.opacity(0.85))
                    .on_click(cx.listener(|this, _, window, cx| this.send(&Send, window, cx)))
            })
            .tooltip(move |_, cx| {
                Tooltip::for_action_in(
                    if is_working {
                        t!("Queue message")
                    } else {
                        t!("Send message")
                    },
                    &Send,
                    &focus_handle,
                    cx,
                )
            })
            .child(icon(IconName::AgentSend, px(14.), plate))
            .into_any_element()
    }

    fn render_attach_button(&self, dictating: bool, cx: &Context<Self>) -> impl IntoElement {
        let hover = ink(0.10, cx);
        let muted = cx.theme().colors().text_muted;
        div()
            .id("agent-attach")
            .size(px(ACTION_BUTTON_SIZE))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .tooltip(Tooltip::text(if dictating {
                t!("Cancel dictation")
            } else {
                t!("Attach images")
            }))
            .child(if dictating {
                icon(IconName::AgentClose, px(16.), muted)
            } else {
                icon(IconName::Plus, px(18.), muted)
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                if dictating {
                    this.cancel_dictation(cx);
                } else {
                    this.pick_images(window, cx);
                }
            }))
    }

    pub(super) fn render_pill(
        &self,
        radius: f32,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let show_commands = self.command_menu.is_some()
            && self.composer.focus_handle(cx).is_focused(window);
        let mut key_context = KeyContext::default();
        key_context.add("AgentComposer");
        if AgentChatSettings::get_global(cx).send_with == AgentChatSendKey::ModifierEnter {
            key_context.add("send_with_modifier");
        }
        if show_commands {
            key_context.add("command_menu");
        }
        let new_chat = {
            let session = self.session.read(cx);
            session.entries().is_empty() && !session.is_working()
        };
        let dictating = self.dictation_active();
        let microphone = self.render_dictation_button(cx);
        let voice_track = microphone
            .is_some()
            .then(|| self.render_voice_track(cx))
            .flatten();
        let model_picker = voice_track.is_none().then(|| {
            div()
                .min_w_0()
                .max_w(gpui::relative(MODEL_CHIP_MAX_FRACTION))
                .child(self.render_model_chip(window, cx))
        });
        let trailing = h_flex()
            .min_w_0()
            .gap(px(ACTION_PRIMARY_GAP))
            .children(voice_track)
            .children(microphone)
            .child(self.render_send_button(cx));
        let textarea_min = if new_chat { NEW_CHAT_TEXTAREA_MIN } else { 0. };
        let pill = div()
            .key_context(key_context)
            .relative()
            .capture_action(cx.listener(|this, _: &editor::actions::Paste, _, cx| {
                if this.paste_images(cx) {
                    cx.stop_propagation();
                }
            }))
            .when(show_commands, |this| this.child(self.render_command_menu(cx)))
            .w_full()
            .rounded(px(radius))
            .border_1()
            .border_color(colors.border)
            .bg(composer_surface(cx))
            .shadow_lg()
            .flex()
            .flex_col()
            .children(self.render_attachment_strip(cx))
            .child(
                div()
                    .flex_none()
                    .px(px(16.))
                    .pt(px(16.))
                    .pb(px(10.))
                    .min_h(px(textarea_min))
                    .max_h(px(TEXTAREA_MAX))
                    .child(self.composer.clone()),
            )
            .child(
                h_flex()
                    .h(px(ACTIONS_ROW_HEIGHT))
                    .pt(px(2.))
                    .pb(px(8.))
                    .px(px(ACTION_INSET))
                    .gap(px(ACTION_PRIMARY_GAP))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(ACTION_PRIMARY_GAP))
                            .child(self.render_attach_button(dictating, cx))
                            .children(model_picker),
                    )
                    .child(trailing),
            );
        v_flex()
            .w_full()
            .gap(px(8.))
            .child(pill)
            .children(self.render_dictation_status(cx))
            .into_any_element()
    }

    fn footer_label_color(cx: &App) -> Hsla {
        cx.theme().colors().text_muted.opacity(0.6)
    }

    fn footer_label_row(leading: impl IntoElement, cx: &App) -> gpui::Div {
        h_flex()
            .h(px(20.))
            .min_w_0()
            .px(px(8.))
            .gap(px(6.))
            .text_size(ui(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(Self::footer_label_color(cx))
            .child(leading)
    }

    pub(super) fn footer_label_shell(icon_name: IconName, cx: &App) -> gpui::Div {
        Self::footer_label_row(icon(icon_name, px(12.), Self::footer_label_color(cx)), cx)
    }

    pub(super) fn render_agent_update(&self, cx: &App) -> Option<AnyElement> {
        let kind = self.session.read(cx).kind();
        let status = self.store.read(cx).agent_update(kind)?.clone();
        let color = Self::footer_label_color(cx);
        let label = |leading: AnyElement, text: SharedString| {
            Self::footer_label_row(leading, cx)
                .max_w(px(280.))
                .child(div().min_w_0().truncate().child(text))
        };
        let status_icon = |icon_name| icon(icon_name, px(12.), color).into_any_element();
        Some(match status {
            AgentUpdateStatus::Updating => label(
                icon(IconName::LoadCircle, px(12.), color)
                    .with_animation(
                        "agent-update-spinner",
                        working_animation(Duration::from_secs(1)),
                        |this, delta| {
                            this.with_transformation(Transformation::rotate(gpui::percentage(
                                delta,
                            )))
                        },
                    )
                    .into_any_element(),
                t!("Updating {agent}…", agent = kind.label()),
            )
            .into_any_element(),
            AgentUpdateStatus::Finished(message) => {
                label(status_icon(IconName::AgentCheck), message).into_any_element()
            }
            AgentUpdateStatus::Failed { message, reason } => {
                label(status_icon(IconName::AgentDangerTriangle), message)
                    .id("agent-update-failed")
                    .tooltip(Tooltip::text(reason))
                    .into_any_element()
            }
        })
    }

    pub(super) fn render_move_to_panel(&self, cx: &Context<Self>) -> Option<impl IntoElement> {
        if self.docked
            || self.subagent.is_some()
            || !AgentChatSettings::get_global(cx).move_to_panel_button
        {
            return None;
        }
        let color = cx.theme().colors().text_muted.opacity(0.8);
        Some(
            Chip::new(
                "agent-move-to-panel",
                6.,
                div()
                    .size(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(IconName::Split, px(14.), color)),
            )
            .hover_background(ink(0.05, cx))
            .tooltip(t!("Move to Side Panel"))
            .on_click(cx.listener(|this, _, window, cx| {
                this.move_to_panel(&crate::MoveChatToPanel, window, cx)
            })),
        )
    }

    pub(super) fn render_footer(&self, cx: &Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        let kind = session.kind();
        let context = session.context();
        let context_fraction = context
            .and_then(|context| context.fraction())
            .map(|fraction| fraction as f32);
        let usage_fraction = self
            .store
            .read(cx)
            .plan_usage(kind)
            .and_then(|state| state.usage.as_ref())
            .and_then(|usage| usage.used_fraction());
        let store = self.store.clone();
        let ring_hover = ink(0.05, cx);
        h_flex()
            .h(px(24.))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .pl(px(10.))
                    .children(self.render_agent_update(cx)),
            )
            .children(self.render_background_indicator(cx))
            .child(
                h_flex()
                    .flex_none()
                    .pl(px(4.))
                    .pr(px(10.))
                    .gap(px(4.))
                    .when(kind.has_plan_usage(), |this| {
                        this.child(
                            PopoverMenu::new(SharedString::from(format!(
                                "agent-plan-usage-{}",
                                cx.entity_id()
                            )))
                            .trigger(
                                Chip::new(
                                    "agent-plan-usage-chip",
                                    6.,
                                    usage_chip(usage_fraction, cx),
                                )
                                .hover_background(ring_hover),
                            )
                            .menu(move |_, cx| {
                                let store = store.clone();
                                Some(cx.new(|cx| UsageCard::new(store, kind, cx)))
                            })
                            .anchor(Anchor::BottomRight)
                            .attach(Anchor::TopRight)
                            .offset(point(px(0.), px(-6.))),
                        )
                    })
                    .child(
                        PopoverMenu::new(SharedString::from(format!(
                            "agent-context-usage-{}",
                            cx.entity_id()
                        )))
                        .trigger(
                            Chip::new(
                                "agent-context-chip",
                                6.,
                                context_chip(context_fraction, cx),
                            )
                            .hover_background(ring_hover),
                        )
                        .menu(move |_, cx| Some(cx.new(|cx| ContextCard::new(context, cx))))
                        .anchor(Anchor::BottomRight)
                        .attach(Anchor::TopRight)
                        .offset(point(px(0.), px(-6.))),
                    )
                    .children(self.render_move_to_panel(cx)),
            )
    }

    pub(super) fn render_status_strip(&self, cx: &Context<Self>) -> impl IntoElement {
        let failed = self.session.read(cx).run_failed();
        h_flex()
            .h(px(24.))
            .px(px(24.))
            .text_size(ui(11.))
            .when(failed, |this| {
                this.text_color(cx.theme().status().error)
                    .child(t!("Run failed"))
            })
    }

    pub(super) fn render_scroll_button(&self, cx: &Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let hover_background = colors.elevated_surface_background.blend(ink(0.08, cx));
        let hover_border = colors.border.blend(ink(0.12, cx));
        h_flex()
            .absolute()
            .bottom(px(6.))
            .left_0()
            .right(px(10.))
            .justify_center()
            .child(
                h_flex()
                    .id("agent-scroll-to-bottom")
                    .h(px(30.))
                    .pl(px(11.))
                    .pr(px(13.))
                    .gap(px(6.))
                    .rounded_full()
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.elevated_surface_background)
                    .shadow_md()
                    .cursor_pointer()
                    .text_size(ui(13.))
                    .hover(move |style| style.bg(hover_background).border_color(hover_border))
                    .child(div().text_color(colors.text_muted).child("↓"))
                    .child(div().text_color(colors.text).child(t!("Scroll to bottom")))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_scroll_button = false;
                        this.list.set_follow_mode(gpui::FollowMode::Tail);
                        cx.notify();
                    })),
            )
    }

    pub(super) fn pick_option(
        &mut self,
        pending_id: SharedString,
        question_id: String,
        option: String,
        multi_select: bool,
        question_count: usize,
        cx: &mut Context<Self>,
    ) {
        let labels = self.selected_options.entry(question_id).or_default();
        if multi_select {
            if let Some(position) = labels.iter().position(|label| *label == option) {
                labels.remove(position);
            } else {
                labels.push(option);
            }
            cx.notify();
            return;
        }
        *labels = vec![option];
        cx.notify();
        self._auto_advance = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AUTO_ADVANCE).await;
            this.update(cx, |this, cx| {
                let page = this
                    .question_pages
                    .get(pending_id.as_ref())
                    .copied()
                    .unwrap_or(0);
                if page + 1 < question_count {
                    this.question_pages.insert(pending_id.to_string(), page + 1);
                    cx.notify();
                } else {
                    this.submit_answers(&pending_id, cx);
                }
            })
            .ok();
        });
    }

    pub(super) fn decide_permission_card(
        &mut self,
        pending_id: &str,
        question_id: &str,
        choice: PermissionChoice,
        cx: &mut Context<Self>,
    ) {
        let Some(question) = self
            .session
            .read(cx)
            .pending_questions()
            .iter()
            .flat_map(|pending| &pending.questions)
            .find(|question| question.id == question_id)
        else {
            return;
        };
        let prompt = question.permission.clone();
        let editors = self.permission_editors.get(question_id);
        let destination = editors.map_or_else(
            || "session".to_string(),
            |editors| editors.destination.clone(),
        );
        let command = editors
            .and_then(|editors| editors.command.as_ref())
            .map(|editor| editor.read(cx).text(cx));
        let reason = editors.map_or_else(String::new, |editors| editors.reason.read(cx).text(cx));
        // An emptied command editor must not silently run the original command.
        if choice != PermissionChoice::Deny
            && command
                .as_deref()
                .is_some_and(|command| command.trim().is_empty())
        {
            return;
        }
        let decision = permission_decision(
            prompt.as_ref(),
            choice,
            &destination,
            command.as_deref(),
            &reason,
        );
        if choice == PermissionChoice::AlwaysAllow {
            self.store.update(cx, |store, cx| {
                store
                    .update_list_prefs(|prefs| prefs.permission_destination = Some(destination), cx)
            });
        }
        self.session.update(cx, |session, cx| {
            session.decide_permission(pending_id, decision, cx)
        });
    }

    fn cycle_permission_destination(&mut self, question_id: &str, cx: &mut Context<Self>) {
        let Some(editors) = self.permission_editors.get_mut(question_id) else {
            return;
        };
        let next = PERMISSION_DESTINATIONS
            .iter()
            .position(|destination| *destination == editors.destination)
            .map_or(0, |position| (position + 1) % PERMISSION_DESTINATIONS.len());
        editors.destination = PERMISSION_DESTINATIONS[next].to_string();
        cx.notify();
    }

    pub(super) fn submit_answer(
        &mut self,
        _: &SubmitAnswer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.session.read(cx).pending_questions().first() else {
            return;
        };
        if pending.is_permission() {
            let pending_id = pending.id().to_string();
            let Some(question_id) = pending
                .questions
                .first()
                .map(|question| question.id.clone())
            else {
                return;
            };
            let Some(editors) = self.permission_editors.get(&question_id) else {
                return;
            };
            if pending.is_plan() {
                if editors.reason.focus_handle(cx).is_focused(window)
                    && !editors.reason.read(cx).text(cx).trim().is_empty()
                {
                    let choice = self.keep_planning(&question_id, cx);
                    self.decide_plan_card(&pending_id, choice, cx);
                }
                return;
            }
            let choice = if editors.reason.focus_handle(cx).is_focused(window) {
                if editors.reason.read(cx).text(cx).trim().is_empty() {
                    return;
                }
                PermissionChoice::Deny
            } else if editors
                .command
                .as_ref()
                .is_some_and(|editor| editor.focus_handle(cx).is_focused(window))
            {
                PermissionChoice::Allow
            } else {
                return;
            };
            self.decide_permission_card(&pending_id, &question_id, choice, cx);
            return;
        }
        let pending_id = pending.id().to_string();
        let count = pending.questions.len();
        let page_index = self
            .question_pages
            .get(&pending_id)
            .copied()
            .unwrap_or(0)
            .min(count.saturating_sub(1));
        let Some(question) = pending.questions.get(page_index) else {
            return;
        };
        if self.answer_labels(question, cx).is_empty() {
            return;
        }
        self.advance_question(&pending_id, page_index, count, cx);
    }

    fn advance_question(
        &mut self,
        pending_id: &str,
        page_index: usize,
        count: usize,
        cx: &mut Context<Self>,
    ) {
        if page_index + 1 >= count {
            self.submit_answers(pending_id, cx);
        } else {
            self.question_pages
                .insert(pending_id.to_string(), page_index + 1);
            cx.notify();
        }
    }

    pub(super) fn submit_answers(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(pending) = self
            .session
            .read(cx)
            .pending_questions()
            .iter()
            .find(|pending| pending.id() == id)
        else {
            return;
        };
        let answers: Vec<UserInputAnswer> = pending
            .questions
            .iter()
            .map(|question| UserInputAnswer {
                question_id: question.id.clone(),
                labels: self.answer_labels(question, cx),
                decision: None,
            })
            .collect();
        for answer in &answers {
            self.selected_options.remove(&answer.question_id);
        }
        self.question_pages.remove(id);
        self.session
            .update(cx, |session, cx| session.answer(id, answers, cx));
    }

    pub(super) fn render_option_row(
        &self,
        key: SharedString,
        number: usize,
        label: AnyElement,
        picked: bool,
        cx: &App,
    ) -> gpui::Stateful<gpui::Div> {
        let colors = cx.theme().colors();
        let hover = ink(0.06, cx);
        h_flex()
            .id(ElementId::Name(key))
            .px(px(14.))
            .py(px(10.))
            .gap(px(12.))
            .rounded(px(12.))
            .border_1()
            .border_color(if picked {
                ink(0.16, cx)
            } else {
                gpui::transparent_black()
            })
            .bg(if picked {
                ink(0.09, cx)
            } else {
                ink(0.025, cx)
            })
            .when(!picked, |this| this.hover(move |style| style.bg(hover)))
            .cursor_pointer()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(ui(13.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if picked {
                        colors.text
                    } else {
                        colors.text.opacity(0.9)
                    })
                    .child(label),
            )
            .when(number <= 9, |this| {
                this.child(
                    div()
                        .flex_none()
                        .size(px(22.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .bg(if picked { ink(0.16, cx) } else { ink(0.05, cx) })
                        .text_size(ui(11.))
                        .text_color(if picked {
                            colors.text
                        } else {
                            colors.text_muted.opacity(0.6)
                        })
                        .child(number.to_string()),
                )
            })
    }

    fn render_question_text(&self, id: &str, text: &str, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let collapsible = text.lines().count() > QUESTION_COLLAPSED_LINES
            || text.chars().count() > QUESTION_COLLAPSE_CHARS;
        let expanded = self.expanded_questions.contains(id);
        let expanded_max_height = self
            .root_bounds
            .get()
            .map_or(px(QUESTION_EXPANDED_FALLBACK), |bounds| {
                bounds.size.height * QUESTION_EXPANDED_FRACTION
            });
        let toggle_group: SharedString = format!("agent-question-toggle-{id}").into();
        let toggle_id = id.to_string();
        v_flex()
            .mt(px(6.))
            .text_size(ui(15.))
            .line_height(px(QUESTION_LINE_HEIGHT))
            .font_weight(FontWeight::MEDIUM)
            .text_color(colors.text)
            // Character counts can't predict wrapping, so the text scrolls whatever its length.
            .child(
                div()
                    .id(SharedString::from(format!("agent-question-text-{id}")))
                    .max_h(if collapsible && !expanded {
                        px(QUESTION_LINE_HEIGHT * QUESTION_COLLAPSED_LINES as f32)
                    } else {
                        expanded_max_height
                    })
                    .overflow_y_scroll()
                    .child(text.to_string()),
            )
            .when(collapsible, |this| {
                this.child(
                    h_flex()
                        .id(SharedString::from(format!("agent-question-expand-{id}")))
                        .group(toggle_group.clone())
                        .mt(px(6.))
                        .gap(px(5.))
                        .text_size(ui(13.))
                        .font_weight(FontWeight::NORMAL)
                        .text_color(colors.text_muted)
                        .cursor_pointer()
                        .hover(|style| style.text_color(colors.text))
                        .child(if expanded {
                            t!("Show less")
                        } else {
                            t!("Show all")
                        })
                        .child(
                            super::icon(
                                if expanded {
                                    IconName::AgentArrowUp
                                } else {
                                    IconName::AgentArrowDown
                                },
                                12.,
                            )
                            .text_color(colors.text_muted)
                            .group_hover(toggle_group, |style| style.text_color(colors.text)),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.expanded_questions.remove(&toggle_id) {
                                this.expanded_questions.insert(toggle_id.clone());
                            }
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }

    pub(super) fn render_question_panel(&self, pending: &PendingQuestion, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let pending_id: SharedString = pending.id().to_string().into();
        let count = pending.questions.len();
        let page_index = self
            .question_pages
            .get(pending_id.as_ref())
            .copied()
            .unwrap_or(0)
            .min(count.saturating_sub(1));
        let Some(question) = pending.questions.get(page_index) else {
            return div().into_any_element();
        };
        let is_permission = pending.is_permission();
        let selected = self
            .selected_options
            .get(&question.id)
            .cloned()
            .unwrap_or_default();
        let typed_editor = self
            .answer_editors
            .get(&question.id)
            .map(|(editor, _)| editor.clone());
        let answer_editor = typed_editor
            .clone()
            .filter(|_| question.options.is_empty());
        let permission_editors = self
            .permission_editors
            .get(&question.id)
            .filter(|_| question.permission.is_some());
        let options: Vec<AnyElement> = if is_permission {
            let always_destination = permission_editors
                .filter(|_| {
                    question
                        .permission
                        .as_ref()
                        .is_some_and(|prompt| prompt.allows_always)
                })
                .map(|editors| editors.destination.clone());
            let mut choices = vec![(t!("Allow").into_any_element(), PermissionChoice::Allow)];
            if let Some(destination) = always_destination {
                let question_id = question.id.clone();
                choices.push((
                    h_flex()
                        .gap(px(8.))
                        .child(t!(
                            "Always allow for {destination}",
                            destination = destination_label(&destination)
                        ))
                        .child(
                            div()
                                .id(SharedString::from(format!(
                                    "permission-destination-{question_id}"
                                )))
                                .px(px(6.))
                                .rounded(px(6.))
                                .text_color(colors.text_muted)
                                .hover(|style| style.text_color(colors.text))
                                .tooltip(Tooltip::text(t!("Change where the rule is saved")))
                                .child(super::icon(IconName::AgentArrowDown, 12.))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.cycle_permission_destination(&question_id, cx)
                                })),
                        )
                        .into_any_element(),
                    PermissionChoice::AlwaysAllow,
                ));
            }
            choices.push((t!("Deny").into_any_element(), PermissionChoice::Deny));
            let command_empty = permission_editors
                .and_then(|editors| editors.command.as_ref())
                .is_some_and(|editor| editor.read(cx).text(cx).trim().is_empty());
            choices
                .into_iter()
                .enumerate()
                .map(|(position, (label, choice))| {
                    let pending_id = pending_id.clone();
                    let question_id = question.id.clone();
                    let blocked = command_empty && choice != PermissionChoice::Deny;
                    self.render_option_row(
                        format!("permission-{pending_id}-{position}").into(),
                        position + 1,
                        label,
                        false,
                        cx,
                    )
                    .when(blocked, |row| row.opacity(0.4))
                    .when(!blocked, |row| {
                        row.on_click(cx.listener(move |this, _, _, cx| {
                            this.decide_permission_card(&pending_id, &question_id, choice, cx)
                        }))
                    })
                    .into_any_element()
                })
                .collect()
        } else {
            question
                .options
                .iter()
                .enumerate()
                .map(|(position, option)| {
                    let picked = selected.contains(option);
                    let pending_id = pending_id.clone();
                    let question_id = question.id.clone();
                    let option = option.clone();
                    let multi_select = question.multi_select;
                    self.render_option_row(
                        format!("option-{}-{position}", question.id).into(),
                        position + 1,
                        option.clone().into_any_element(),
                        picked,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.pick_option(
                            pending_id.clone(),
                            question_id.clone(),
                            option.clone(),
                            multi_select,
                            count,
                            cx,
                        )
                    }))
                    .into_any_element()
                })
                .chain(
                    typed_editor
                        .filter(|_| !question.options.is_empty())
                        .map(|editor| {
                            let typed = !editor.read(cx).text(cx).trim().is_empty()
                                && (question.multi_select || selected.is_empty());
                            let focus = editor.focus_handle(cx);
                            self.render_option_row(
                                format!("option-{}-typed", question.id).into(),
                                question.options.len() + 1,
                                editor.into_any_element(),
                                typed,
                                cx,
                            )
                            .cursor_text()
                            .on_click(move |_, window, cx| window.focus(&focus, cx))
                            .into_any_element()
                        }),
                )
                .collect()
        };
        let answered = !self.answer_labels(question, cx).is_empty();
        let is_last_page = page_index + 1 >= count;
        let header = if is_permission {
            t!("Permission").to_string()
        } else if question.header == agent_harness::claude::QUESTION_HEADER {
            i18n::translate(agent_harness::claude::QUESTION_HEADER).to_string()
        } else {
            question.header.clone()
        };
        let muted_label = colors.text_muted.opacity(0.6);
        let back_hover = ink(0.06, cx);
        let options_max_height = self
            .root_bounds
            .get()
            .map_or(px(QUESTION_EXPANDED_FALLBACK), |bounds| {
                bounds.size.height * QUESTION_OPTIONS_FRACTION
            });
        v_flex()
            .track_focus(&self.question_focus)
            .key_context(question_key_context(cx))
            .w_full()
            .rounded(px(COMPOSER_RADIUS))
            .border_1()
            .border_color(colors.border)
            .bg(composer_surface(cx))
            .shadow_lg()
            .child(
                v_flex()
                    .px(px(16.))
                    .pt(px(16.))
                    .child(
                        h_flex()
                            .gap(px(10.))
                            .child(
                                div()
                                    .text_size(ui(10.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(muted_label)
                                    .child(tracked_upper(&header)),
                            )
                            .when(count > 1, |this| {
                                this.child(
                                    div()
                                        .h(px(20.))
                                        .px(px(6.))
                                        .flex()
                                        .items_center()
                                        .rounded(px(6.))
                                        .bg(ink(0.06, cx))
                                        .text_size(ui(10.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(muted_label)
                                        .child(t!(
                                            "{current} of {count}",
                                            current = page_index + 1,
                                            count = count
                                        )),
                                )
                            }),
                    )
                    .child(self.render_question_text(&question.id, &question.question, cx))
                    .when_some(
                        permission_editors.and_then(|editors| editors.command.clone()),
                        |this, editor| {
                            this.child(
                                div()
                                    .mt(px(8.))
                                    .px(px(10.))
                                    .py(px(6.))
                                    .rounded(px(8.))
                                    .border_1()
                                    .border_color(hairline(0.06, cx))
                                    .bg(ink(0.025, cx))
                                    .child(editor),
                            )
                        },
                    )
                    .when(question.multi_select, |this| {
                        this.child(
                            div()
                                .mt(px(4.))
                                .text_size(ui(12.))
                                .text_color(colors.text_muted.opacity(0.65))
                                .child(t!("Select one or more options.")),
                        )
                    })
                    .child(
                        v_flex()
                            .id("agent-question-options")
                            .mt(px(12.))
                            .gap(px(4.))
                            .max_h(options_max_height)
                            .overflow_y_scroll()
                            .children(options),
                    )
                    .when_some(
                        permission_editors.map(|editors| editors.reason.clone()),
                        |this, editor| {
                            this.child(
                                div()
                                    .mt(px(12.))
                                    .border_t_1()
                                    .border_color(hairline(0.06, cx))
                                    .pt(px(12.))
                                    .pb(px(4.))
                                    .px(px(4.))
                                    .child(editor),
                            )
                        },
                    )
                    .when_some(answer_editor, |this, editor| {
                        this.child(
                            div()
                                .mt(px(12.))
                                .border_t_1()
                                .border_color(hairline(0.06, cx))
                                .pt(px(12.))
                                .pb(px(4.))
                                .px(px(4.))
                                .child(editor),
                        )
                    }),
            )
            .child(
                h_flex()
                    .px(px(16.))
                    .pt(px(4.))
                    .pb(px(16.))
                    .justify_between()
                    .child(if page_index > 0 {
                        let pending_id = pending_id.clone();
                        div()
                            .id("agent-question-back")
                            .px(px(12.))
                            .py(px(6.))
                            .rounded(px(8.))
                            .cursor_pointer()
                            .text_size(ui(13.))
                            .text_color(colors.text_muted)
                            .hover(move |style| style.bg(back_hover).text_color(colors.text))
                            .child(t!("Back"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.question_pages
                                    .insert(pending_id.to_string(), page_index - 1);
                                cx.notify();
                            }))
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    })
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(self.render_question_stop(cx))
                            .when(!is_permission, |this| {
                                let pending_id = pending_id.clone();
                                this.child(
                                    div()
                                        .id("agent-question-next")
                                        .px(px(16.))
                                        .py(px(6.))
                                        .rounded(px(8.))
                                        .bg(colors.text)
                                        .text_size(ui(13.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(page(cx))
                                        .when(!answered, |this| this.opacity(0.4))
                                        .when(answered, |this| {
                                            this.cursor_pointer()
                                                .hover(|style| style.opacity(0.9))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.advance_question(&pending_id, page_index, count, cx)
                                                }))
                                        })
                                        .child(if is_last_page {
                                            t!("Submit")
                                        } else {
                                            t!("Next")
                                        }),
                                )
                            })
                    ),
            )
            .into_any_element()
    }

    fn render_question_stop(&self, cx: &Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let hover = ink(0.06, cx);
        let question_focus = self.question_focus.clone();
        div()
            .id("agent-question-stop")
            .px(px(12.))
            .py(px(6.))
            .rounded(px(8.))
            .cursor_pointer()
            .text_size(ui(13.))
            .text_color(colors.text_muted)
            .hover(move |style| style.bg(hover).text_color(colors.text))
            .tooltip(move |_, cx| Tooltip::for_action_in(t!("Stop"), &Stop, &question_focus, cx))
            .child(t!("Stop"))
            .on_click(cx.listener(|this, _, window, cx| this.stop(&Stop, window, cx)))
    }

    fn keep_planning(&self, question_id: &str, cx: &App) -> PlanChoice {
        let editors = self.permission_editors.get(question_id);
        PlanChoice::KeepPlanning {
            feedback: editors.map_or_else(String::new, |editors| editors.reason.read(cx).text(cx)),
            comments: editors.map_or_else(Vec::new, |editors| editors.plan_comments.clone()),
        }
    }

    /// The quote is the plan text selected last; the comment is what was typed in the feedback editor.
    fn comment_on_plan(&mut self, question_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editors) = self.permission_editors.get_mut(question_id) else {
            return;
        };
        let text = editors.reason.read(cx).text(cx).trim().to_string();
        if text.is_empty() {
            window.focus(&editors.reason.focus_handle(cx), cx);
            return;
        }
        let Some(quote) = editors.plan_quote.take() else {
            return;
        };
        editors.plan_comments.push(PlanComment { quote, text });
        editors
            .reason
            .update(cx, |editor, cx| editor.clear(window, cx));
        cx.notify();
    }

    fn dismiss_plan_quote(&mut self, question_id: &str, cx: &mut Context<Self>) {
        if let Some(editors) = self.permission_editors.get_mut(question_id)
            && editors.plan_quote.take().is_some()
        {
            cx.notify();
        }
    }

    fn remove_plan_comment(&mut self, question_id: &str, index: usize, cx: &mut Context<Self>) {
        if let Some(editors) = self.permission_editors.get_mut(question_id)
            && index < editors.plan_comments.len()
        {
            editors.plan_comments.remove(index);
            cx.notify();
        }
    }

    pub(super) fn open_plan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let plan = self.session.update(cx, |session, cx| {
            session.control(serde_json::json!({ "subtype": "get_plan" }), cx)
        });
        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |_, cx| {
            let message = match plan.await {
                Ok(plan) => match plan["path"].as_str().filter(|_| plan["exists"] == true) {
                    Some(path) => {
                        let opening = workspace.update_in(cx, |workspace, window, cx| {
                            workspace.open_abs_path(
                                path.into(),
                                OpenOptions {
                                    visible: Some(OpenVisible::None),
                                    ..Default::default()
                                },
                                window,
                                cx,
                            )
                        })?;
                        match opening.await {
                            Ok(_) => return Ok(()),
                            Err(error) => {
                                t!("Couldn't open the plan: {error}", error = error).to_string()
                            }
                        }
                    }
                    None => t!("No plan yet").to_string(),
                },
                Err(error) => error.to_string(),
            };
            workspace.update(cx, |workspace, cx| {
                workspace.show_toast(
                    Toast::new(NotificationId::unique::<ChatView>(), message).autohide(),
                    cx,
                )
            })
        })
        .detach_and_log_err(cx);
    }

    fn render_plan_comments(&self, question_id: &str, cx: &Context<Self>) -> Option<AnyElement> {
        let colors = cx.theme().colors();
        let editors = self.permission_editors.get(question_id)?;
        if editors.plan_comments.is_empty() {
            return None;
        }
        let rows = editors
            .plan_comments
            .iter()
            .enumerate()
            .map(|(index, comment)| {
                let question_id = question_id.to_string();
                let quote: String = comment.quote.chars().take(PLAN_QUOTE_CHARS).collect();
                let ellipsis = if quote.len() < comment.quote.len() {
                    "…"
                } else {
                    ""
                };
                h_flex()
                    .gap(px(8.))
                    .items_start()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_color(colors.text_muted)
                                    .child(format!("\"{quote}{ellipsis}\"")),
                            )
                            .child(comment.text.clone()),
                    )
                    .child(
                        IconButton::new(("agent-plan-comment-remove", index), IconName::Close)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text(t!("Remove comment")))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_plan_comment(&question_id, index, cx)
                            })),
                    )
            });
        Some(
            v_flex()
                .mt(px(8.))
                .gap(px(6.))
                .text_size(ui(13.))
                .child(
                    div()
                        .text_size(ui(10.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors.text_muted.opacity(0.6))
                        .child(tracked_upper(&t!(
                            "Comments ({count})",
                            count = editors.plan_comments.len()
                        ))),
                )
                .children(rows)
                .into_any_element(),
        )
    }

    fn render_plan_actions(&self, question_id: &str, cx: &Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let quote = self
            .permission_editors
            .get(question_id)
            .and_then(|editors| editors.plan_quote.clone());
        let text_button = |id: &'static str, label: SharedString| {
            let hover = ink(0.06, cx);
            div()
                .id(id)
                .flex_none()
                .px(px(8.))
                .py(px(4.))
                .rounded(px(6.))
                .cursor_pointer()
                .text_size(ui(12.5))
                .text_color(colors.text_muted)
                .hover(move |style| style.bg(hover).text_color(colors.text))
                .child(label)
        };
        h_flex()
            .mt(px(6.))
            .gap(px(8.))
            .child(
                text_button("agent-plan-open", t!("Open plan"))
                    .on_click(cx.listener(|this, _, window, cx| this.open_plan(window, cx))),
            )
            .child(div().flex_1())
            .when_some(quote, |this, quote| {
                let question_id = question_id.to_string();
                this.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(ui(12.5))
                        .text_color(colors.text_muted)
                        .child(format!("\"{quote}\"")),
                )
                .child(text_button("agent-plan-comment", t!("Comment")).on_click({
                    let question_id = question_id.clone();
                    cx.listener(move |this, _, window, cx| {
                        this.comment_on_plan(&question_id, window, cx)
                    })
                }))
                .child(
                    IconButton::new("agent-plan-quote-dismiss", IconName::Close)
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text(t!("Dismiss")))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.dismiss_plan_quote(&question_id, cx)
                        })),
                )
            })
    }

    fn decide_plan_card(&mut self, pending_id: &str, choice: PlanChoice, cx: &mut Context<Self>) {
        let (mode, decision) = plan_decision(&choice);
        self.session.update(cx, |session, cx| {
            session.decide_plan(pending_id, mode, decision, cx)
        });
    }

    pub(super) fn render_plan_panel(
        &self,
        pending: &PendingQuestion,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let pending_id: SharedString = pending.id().to_string().into();
        let Some(question) = pending.questions.first() else {
            return div().into_any_element();
        };
        let editors = self.permission_editors.get(&question.id);
        let has_comments = editors.is_some_and(|editors| !editors.plan_comments.is_empty());
        let deciding = self.session.read(cx).is_deciding_plan(&pending_id);
        let mut choices = Vec::new();
        // Comments only reach Claude with "keep planning", so approving would drop them.
        if !has_comments {
            choices.push(if self.session.read(cx).plan_approval_uses_auto() {
                (t!("Yes, and use auto mode"), Some(PlanChoice::Auto))
            } else {
                (
                    t!("Yes, and auto-accept edits"),
                    Some(PlanChoice::AcceptEdits),
                )
            });
            choices.push((
                t!("Yes, and manually approve edits"),
                Some(PlanChoice::Manual),
            ));
        }
        choices.push((t!("No, keep planning"), None));
        let options: Vec<AnyElement> = choices
            .into_iter()
            .enumerate()
            .map(|(position, (label, choice))| {
                let pending_id = pending_id.clone();
                let question_id = question.id.clone();
                self.render_option_row(
                    format!("plan-{pending_id}-{position}").into(),
                    position + 1,
                    label.into_any_element(),
                    false,
                    cx,
                )
                .when(deciding, |this| this.opacity(0.5))
                .when(!deciding, |this| {
                    this.on_click(cx.listener(move |this, _, _, cx| {
                        let choice = choice
                            .clone()
                            .unwrap_or_else(|| this.keep_planning(&question_id, cx));
                        this.decide_plan_card(&pending_id, choice, cx)
                    }))
                })
                .into_any_element()
            })
            .collect();
        let plan_max_height = self
            .root_bounds
            .get()
            .map_or(px(QUESTION_EXPANDED_FALLBACK), |bounds| {
                bounds.size.height * QUESTION_EXPANDED_FRACTION
            });
        v_flex()
            .track_focus(&self.question_focus)
            .key_context(question_key_context(cx))
            .w_full()
            .rounded(px(COMPOSER_RADIUS))
            .border_1()
            .border_color(colors.border)
            .bg(composer_surface(cx))
            .shadow_lg()
            .child(
                v_flex()
                    .px(px(16.))
                    .pt(px(16.))
                    .child(
                        div()
                            .text_size(ui(10.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors.text_muted.opacity(0.6))
                            .child(tracked_upper(&t!("Plan"))),
                    )
                    .child(self.render_question_text(&question.id, &question.question, cx))
                    .when_some(
                        editors.and_then(|editors| editors.plan.clone()),
                        |this, plan| {
                            this.child(
                                div()
                                    .id(SharedString::from(format!("agent-plan-{}", question.id)))
                                    .mt(px(8.))
                                    .px(px(12.))
                                    .py(px(8.))
                                    .max_h(plan_max_height)
                                    .overflow_y_scroll()
                                    .rounded(px(8.))
                                    .border_1()
                                    .border_color(hairline(0.06, cx))
                                    .bg(ink(0.025, cx))
                                    .child(MarkdownElement::new(
                                        plan,
                                        Self::message_style(window, cx),
                                    )),
                            )
                            .child(self.render_plan_actions(&question.id, cx))
                        },
                    )
                    .children(self.render_plan_comments(&question.id, cx))
                    .child(
                        v_flex()
                            .id("agent-plan-options")
                            .mt(px(12.))
                            .gap(px(4.))
                            .children(options),
                    )
                    .when_some(
                        editors.map(|editors| editors.reason.clone()),
                        |this, editor| {
                            this.child(
                                div()
                                    .mt(px(12.))
                                    .border_t_1()
                                    .border_color(hairline(0.06, cx))
                                    .pt(px(12.))
                                    .pb(px(4.))
                                    .px(px(4.))
                                    .child(editor),
                            )
                        },
                    ),
            )
            .child(
                h_flex()
                    .px(px(16.))
                    .pt(px(4.))
                    .pb(px(16.))
                    .justify_end()
                    .child(self.render_question_stop(cx)),
            )
            .into_any_element()
    }
}

fn question_key_context(cx: &App) -> KeyContext {
    let mut key_context = KeyContext::default();
    key_context.add("AgentQuestion");
    if AgentChatSettings::get_global(cx).send_with == AgentChatSendKey::ModifierEnter {
        key_context.add("send_with_modifier");
    }
    key_context
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prompt(input: serde_json::Value, suggestions: Vec<serde_json::Value>) -> PermissionPrompt {
        PermissionPrompt {
            tool_name: "Bash".into(),
            input,
            suggestions,
            allows_always: true,
            ..PermissionPrompt::default()
        }
    }

    #[test]
    fn always_allow_overrides_destination_and_drops_set_mode() {
        let suggestions = vec![
            json!({
                "type": "addRules",
                "rules": [{"toolName": "Bash", "ruleContent": "git status"}],
                "behavior": "allow",
                "destination": "session",
            }),
            json!({"type": "setMode", "mode": "acceptEdits", "destination": "session"}),
        ];
        let updates = always_allow_updates(&suggestions, "localSettings");
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0]["type"], "addRules");
        assert_eq!(updates[0]["destination"], "localSettings");
    }

    #[test]
    fn set_mode_only_keeps_its_own_destination() {
        let suggestions =
            vec![json!({"type": "setMode", "mode": "acceptEdits", "destination": "session"})];
        let updates = always_allow_updates(&suggestions, "localSettings");
        assert_eq!(updates, suggestions);
    }

    #[test]
    fn deny_with_text_uses_reason_template() {
        let prompt = prompt(json!({"command": "npm i"}), Vec::new());
        let decision = permission_decision(
            Some(&prompt),
            PermissionChoice::Deny,
            "session",
            None,
            "  use pnpm ",
        );
        assert_eq!(
            decision,
            PermissionDecision::Deny {
                message: format!("{}use pnpm", agent_harness::claude::DENY_REASON_PREFIX),
                interrupt: false,
            }
        );
    }

    #[test]
    fn deny_without_text_stops() {
        let prompt = prompt(json!({"command": "npm i"}), Vec::new());
        let decision =
            permission_decision(Some(&prompt), PermissionChoice::Deny, "session", None, " ");
        assert_eq!(
            decision,
            PermissionDecision::Deny {
                message: agent_harness::claude::DENY_STOP.into(),
                interrupt: true,
            }
        );
    }

    #[test]
    fn cards_without_a_prompt_still_allow_and_deny() {
        assert_eq!(
            permission_decision(None, PermissionChoice::Allow, "session", None, ""),
            PermissionDecision::Allow {
                updated_input: None,
                updated_permissions: Vec::new(),
            }
        );
        assert_eq!(
            permission_decision(None, PermissionChoice::Deny, "session", None, ""),
            PermissionDecision::Deny {
                message: agent_harness::claude::DENY_STOP.into(),
                interrupt: true,
            }
        );
    }

    #[test]
    fn a_trailing_newline_in_the_original_command_is_not_an_edit() {
        let prompt = prompt(json!({"command": "ls\n"}), Vec::new());
        let decision = permission_decision(
            Some(&prompt),
            PermissionChoice::Allow,
            "session",
            Some("ls"),
            "",
        );
        assert_eq!(
            decision,
            PermissionDecision::Allow {
                updated_input: None,
                updated_permissions: Vec::new(),
            }
        );
    }

    #[test]
    fn edited_command_is_sent_as_updated_input() {
        let prompt = prompt(json!({"command": "ls", "description": "d"}), Vec::new());
        let decision = permission_decision(
            Some(&prompt),
            PermissionChoice::Allow,
            "session",
            Some("ls -la"),
            "",
        );
        assert_eq!(
            decision,
            PermissionDecision::Allow {
                updated_input: Some(json!({"command": "ls -la", "description": "d"})),
                updated_permissions: Vec::new(),
            }
        );
        let unchanged = permission_decision(
            Some(&prompt),
            PermissionChoice::Allow,
            "session",
            Some("ls\n"),
            "",
        );
        assert_eq!(
            unchanged,
            PermissionDecision::Allow {
                updated_input: None,
                updated_permissions: Vec::new(),
            }
        );
    }

    fn allow() -> PermissionDecision {
        PermissionDecision::Allow {
            updated_input: None,
            updated_permissions: Vec::new(),
        }
    }

    fn keep_planning(feedback: &str, comments: &[(&str, &str)]) -> PlanChoice {
        PlanChoice::KeepPlanning {
            feedback: feedback.into(),
            comments: comments
                .iter()
                .map(|(quote, text)| PlanComment {
                    quote: quote.to_string(),
                    text: text.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn accepting_edits_switches_the_mode_and_allows() {
        assert_eq!(
            plan_decision(&PlanChoice::AcceptEdits),
            (Some(PermissionMode::AcceptEdits), allow())
        );
    }

    #[test]
    fn auto_switches_the_mode_and_allows() {
        assert_eq!(
            plan_decision(&PlanChoice::Auto),
            (Some(PermissionMode::Auto), allow())
        );
    }

    #[test]
    fn manual_approval_asks_the_cli_for_default_mode() {
        assert_eq!(
            plan_decision(&PlanChoice::Manual),
            (
                None,
                PermissionDecision::Allow {
                    updated_input: None,
                    updated_permissions: vec![
                        json!({"type": "setMode", "mode": "default", "destination": "session"})
                    ],
                }
            )
        );
    }

    #[test]
    fn keep_planning_without_feedback_stops() {
        assert_eq!(
            plan_decision(&keep_planning("  ", &[])),
            (
                None,
                PermissionDecision::Deny {
                    message: "User chose to stay in plan mode and continue planning".into(),
                    interrupt: true,
                }
            )
        );
    }

    #[test]
    fn keep_planning_with_feedback_sends_the_reason() {
        assert_eq!(
            plan_decision(&keep_planning(" also update the help text ", &[])),
            (
                None,
                PermissionDecision::Deny {
                    message: format!(
                        "{}also update the help text",
                        agent_harness::claude::DENY_REASON_PREFIX
                    ),
                    interrupt: false,
                }
            )
        );
    }

    #[test]
    fn plan_comments_are_appended_one_per_line() {
        let comments = [("step 2", "skip this"), ("tests", "add one for Windows")];
        let list = "\n\nComments on the plan:\n[Re: \"step 2\"] skip this\n[Re: \"tests\"] add one for Windows";
        assert_eq!(
            plan_decision(&keep_planning("", &comments)),
            (
                None,
                PermissionDecision::Deny {
                    message: format!("User chose to stay in plan mode and continue planning{list}"),
                    interrupt: false,
                }
            )
        );
        assert_eq!(
            plan_decision(&keep_planning("shorter", &comments)),
            (
                None,
                PermissionDecision::Deny {
                    message: format!("{}shorter{list}", agent_harness::claude::DENY_REASON_PREFIX),
                    interrupt: false,
                }
            )
        );
    }

    #[gpui::test]
    async fn a_plan_comment_quotes_the_selection_and_takes_the_typed_text(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::session::tests::{new_store, run_until};
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| store.skip_plan_usage());
        let project = project::Project::test(project::FakeFs::new(cx.executor()), [], cx).await;
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let (view, cx) = cx.add_window_view(|window, cx| {
            ChatView::new(
                session.clone(),
                store.clone(),
                project,
                WeakEntity::new_invalid(),
                window,
                cx,
            )
        });
        session.update(cx, |session, cx| {
            session.update_settings(|settings| settings.permission = PermissionMode::Plan, cx);
            session.send_message("scenario:plan-approval", cx);
        });
        run_until(cx, |cx| {
            view.read_with(cx, |view, _| !view.permission_editors.is_empty())
        });
        view.update_in(cx, |view, window, cx| {
            let question_id = view
                .permission_editors
                .keys()
                .next()
                .cloned()
                .expect("the plan card has editors");
            view.comment_on_plan(&question_id, window, cx);
            assert!(
                view.permission_editors[&question_id]
                    .plan_comments
                    .is_empty()
            );

            let editors = view
                .permission_editors
                .get_mut(&question_id)
                .expect("the plan card has editors");
            editors.plan_quote = Some("# Plan".into());
            editors.reason.update(cx, |editor, cx| {
                editor.set_text("make it shorter", window, cx)
            });
            view.comment_on_plan(&question_id, window, cx);
            let comment = PlanComment {
                quote: "# Plan".into(),
                text: "make it shorter".into(),
            };
            let editors = &view.permission_editors[&question_id];
            assert_eq!(editors.plan_comments, std::slice::from_ref(&comment));
            assert!(editors.plan_quote.is_none());
            assert_eq!(editors.reason.read(cx).text(cx), "");
            assert_eq!(
                view.keep_planning(&question_id, cx),
                PlanChoice::KeepPlanning {
                    feedback: String::new(),
                    comments: vec![comment],
                }
            );

            view.remove_plan_comment(&question_id, 0, cx);
            assert!(
                view.permission_editors[&question_id]
                    .plan_comments
                    .is_empty()
            );

            view.permission_editors
                .get_mut(&question_id)
                .expect("the plan card has editors")
                .plan_quote = Some("# Plan".into());
            view.dismiss_plan_quote(&question_id, cx);
            assert!(view.permission_editors[&question_id].plan_quote.is_none());
        });
    }

    #[gpui::test]
    async fn enter_in_the_plan_feedback_keeps_planning_even_with_a_pending_quote(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::session::tests::{new_store, run_until};
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| store.skip_plan_usage());
        let project = project::Project::test(project::FakeFs::new(cx.executor()), [], cx).await;
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let (view, cx) = cx.add_window_view(|window, cx| {
            ChatView::new(
                session.clone(),
                store.clone(),
                project,
                WeakEntity::new_invalid(),
                window,
                cx,
            )
        });
        session.update(cx, |session, cx| {
            session.update_settings(|settings| settings.permission = PermissionMode::Plan, cx);
            session.send_message("scenario:plan-approval", cx);
        });
        run_until(cx, |cx| {
            view.read_with(cx, |view, _| !view.permission_editors.is_empty())
        });
        let question_id = view.update_in(cx, |view, window, cx| {
            let (question_id, editors) = view
                .permission_editors
                .iter_mut()
                .next()
                .expect("the plan card has editors");
            editors.plan_quote = Some("# Plan".into());
            editors.reason.update(cx, |editor, cx| {
                editor.set_text("make it shorter", window, cx)
            });
            window.focus(&editors.reason.focus_handle(cx), cx);
            question_id.clone()
        });
        cx.run_until_parked();
        view.update_in(cx, |view, window, cx| {
            view.submit_answer(&SubmitAnswer, window, cx);
            assert!(
                view.permission_editors[&question_id]
                    .plan_comments
                    .is_empty()
            );
            assert!(view.session.read(cx).pending_questions().is_empty());
        });
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| !session.is_working())
        });
        session.read_with(cx, |session, _| {
            assert_eq!(session.settings().permission, PermissionMode::Plan)
        });
    }
}
