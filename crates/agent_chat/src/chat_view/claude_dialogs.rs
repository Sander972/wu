use super::{ChatView, composer::open_path};
use crate::{
    AgentKind,
    session::{AgentSession, AgentStore, Entry, spawn_agent_work},
    slash_commands::AppCommand,
    usage_rings::{resets_in, with_separators},
};
use editor::Editor;
use gpui::{
    App, AppContext as _, ClipboardItem, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, PromptButton, PromptLevel, Render, ScrollHandle, SharedString, Task, WeakEntity,
    Window, rems,
};
use i18n::t;
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};
use ui::{
    Modal, ModalFooter, ModalHeader, Section, SectionHeader, Switch, ToggleButtonGroup,
    ToggleButtonSimple, ToggleState, prelude::*,
};
use util::ResultExt as _;
use workspace::{ModalView, Toast, Workspace, notifications::NotificationId};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DialogSection {
    pub title: SharedString,
    pub rows: Vec<DialogRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DialogRow {
    pub label: SharedString,
    pub detail: Option<SharedString>,
    pub badge: Option<SharedString>,
    pub actions: Vec<RowAction>,
}

#[derive(Clone, Debug, PartialEq)]
#[expect(
    dead_code,
    reason = "OpenUrl rows come with the dialogs that need them"
)]
pub(crate) enum RowAction {
    OpenPath(PathBuf),
    OpenUrl(String),
    Control {
        label: SharedString,
        request: Value,
    },
    Toggle {
        on: bool,
        request_on: Value,
        request_off: Value,
    },
    Shell {
        label: SharedString,
        arguments: Vec<String>,
        input: Option<String>,
        confirmation: SharedString,
    },
    Authenticate {
        label: SharedString,
        request: Value,
    },
}

impl DialogRow {
    fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            detail: None,
            badge: None,
            actions: Vec::new(),
        }
    }
}

fn text(value: &Value, key: &str) -> Option<SharedString> {
    value[key]
        .as_str()
        .filter(|text| !text.is_empty())
        .map(|text| SharedString::from(text.to_string()))
}

fn items<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value[key].as_array().into_iter().flatten()
}

fn yes_no(value: bool) -> SharedString {
    if value { t!("Yes") } else { t!("No") }
}

fn push_grouped(sections: &mut Vec<DialogSection>, title: SharedString, row: DialogRow) {
    match sections.iter_mut().find(|section| section.title == title) {
        Some(section) => section.rows.push(row),
        None => sections.push(DialogSection {
            title,
            rows: vec![row],
        }),
    }
}

pub(crate) fn status_sections(response: &Value) -> Vec<DialogSection> {
    items(response, "sections")
        .map(|section| DialogSection {
            title: text(section, "title").unwrap_or_default(),
            rows: items(section, "rows")
                .map(|row| match text(row, "label") {
                    Some(label) => DialogRow {
                        detail: text(row, "value"),
                        ..DialogRow::new(label)
                    },
                    None => DialogRow::new(text(row, "value").unwrap_or_default()),
                })
                .collect(),
        })
        .collect()
}

pub(crate) fn hooks_sections(response: &Value) -> Vec<DialogSection> {
    let mut sections = Vec::new();
    for hook in items(response, "hooks") {
        let event = text(hook, "event").unwrap_or_default();
        let summary = items(response, "events")
            .find(|candidate| candidate["name"] == event.as_ref())
            .and_then(|candidate| text(candidate, "summary"));
        let title = match summary {
            Some(summary) => format!("{event} · {summary}").into(),
            None => event,
        };
        let row = DialogRow {
            detail: text(hook, "displayText").or_else(|| text(hook, "commandText")),
            badge: text(hook, "pluginName")
                .or_else(|| text(hook, "sourceLabel"))
                .or_else(|| text(hook, "source")),
            ..DialogRow::new(text(hook, "matcher").unwrap_or_else(|| "*".into()))
        };
        push_grouped(&mut sections, title, row);
    }
    sections
}

pub(crate) fn memory_sections(response: &Value) -> Vec<DialogSection> {
    let open = |entry: &Value| -> Vec<RowAction> {
        entry["path"]
            .as_str()
            .map(|path| RowAction::OpenPath(path.into()))
            .into_iter()
            .collect()
    };
    let files: Vec<DialogRow> = items(response, "files")
        .map(|file| DialogRow {
            detail: text(file, "description"),
            badge: (file["exists"] == false).then(|| t!("Not created yet")),
            actions: open(file),
            ..DialogRow::new(
                text(file, "label")
                    .or_else(|| text(file, "path"))
                    .unwrap_or_default(),
            )
        })
        .collect();
    let mut sections = Vec::new();
    if !files.is_empty() {
        sections.push(DialogSection {
            title: t!("Memory files"),
            rows: files,
        });
    }
    let mut memories = Vec::new();
    let auto_memory = &response["auto_memory"];
    if auto_memory.is_object() {
        let mut row = DialogRow {
            detail: text(auto_memory, "status"),
            ..DialogRow::new(t!("Save memories automatically"))
        };
        if auto_memory["toggleable"] == true {
            let setting = |enabled: bool| {
                json!({
                    "subtype": "update_settings",
                    "source": "userSettings",
                    "settings": { "autoMemoryEnabled": enabled },
                })
            };
            row.actions.push(RowAction::Toggle {
                on: auto_memory["enabled"] == true,
                request_on: setting(true),
                request_off: setting(false),
            });
        }
        memories.push(row);
    }
    memories.extend(items(response, "memories").map(|memory| {
        DialogRow {
            detail: text(memory, "description"),
            actions: open(memory),
            ..DialogRow::new(
                text(memory, "name")
                    .or_else(|| text(memory, "path"))
                    .unwrap_or_default(),
            )
        }
    }));
    if !memories.is_empty() {
        sections.push(DialogSection {
            title: t!("Auto memory"),
            rows: memories,
        });
    }
    sections
}

pub(crate) fn skills_sections(response: &Value) -> Vec<DialogSection> {
    let mut sections = Vec::new();
    for skill in items(response, "skills") {
        let row = DialogRow {
            detail: text(skill, "description"),
            badge: text(skill, "state"),
            ..DialogRow::new(
                text(skill, "display_name")
                    .or_else(|| text(skill, "name"))
                    .unwrap_or_default(),
            )
        };
        push_grouped(
            &mut sections,
            text(skill, "source").unwrap_or_default(),
            row,
        );
    }
    sections
}

pub(crate) fn sandbox_sections(response: &Value) -> Vec<DialogSection> {
    let supported = response["supported"] == true;
    let supported = match text(response, "unsupported_reason").filter(|_| !supported) {
        Some(reason) => format!("{} ({reason})", yes_no(false)).into(),
        None => yes_no(supported),
    };
    let state = [
        (t!("Supported"), Some(supported)),
        (t!("Enabled"), Some(yes_no(response["enabled"] == true))),
        (t!("Mode"), text(response, "mode")),
    ]
    .map(|(label, detail)| DialogRow {
        detail,
        ..DialogRow::new(label)
    });
    let mut sections = vec![DialogSection {
        title: t!("Sandbox"),
        rows: state.into(),
    }];
    let excluded: Vec<DialogRow> = items(response, "excluded_commands")
        .filter_map(Value::as_str)
        .map(|command| DialogRow::new(command.to_string()))
        .collect();
    if !excluded.is_empty() {
        sections.push(DialogSection {
            title: t!("Excluded commands"),
            rows: excluded,
        });
    }
    let restrictions: Vec<DialogRow> = response["restrictions"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(name, values)| {
            let values: Vec<&str> = values
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .collect();
            (!values.is_empty()).then(|| DialogRow {
                detail: Some(values.join(", ").into()),
                ..DialogRow::new(name.clone())
            })
        })
        .collect();
    if !restrictions.is_empty() {
        sections.push(DialogSection {
            title: t!("Restrictions"),
            rows: restrictions,
        });
    }
    sections
}

fn dollars(cost: f64) -> String {
    if cost > 0.0 && cost < 0.01 {
        format!("${cost:.4}")
    } else {
        format!("${cost:.2}")
    }
}

fn duration_text(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    match (seconds / 3600, seconds / 60 % 60, seconds % 60) {
        (0, 0, seconds) => format!("{seconds}s"),
        (0, minutes, seconds) => format!("{minutes}m {seconds}s"),
        (hours, minutes, seconds) => format!("{hours}h {minutes}m {seconds}s"),
    }
}

fn scope_name(scope: &Value) -> Option<&str> {
    scope["model"]["display_name"]
        .as_str()
        .or_else(|| scope["surface"].as_str())
}

pub(crate) fn usage_sections(response: &Value) -> Vec<DialogSection> {
    usage_sections_at(response, chrono::Utc::now())
}

fn usage_sections_at(response: &Value, now: chrono::DateTime<chrono::Utc>) -> Vec<DialogSection> {
    let reset_detail = |resets_at: &Value| {
        resets_at
            .as_str()
            .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
            .and_then(|at| resets_in(at.into(), now))
            .map(SharedString::from)
    };
    // `limits[]` is the CLI's newer list and also carries the model-scoped windows, so it replaces the fixed windows when present.
    let mut limits: Vec<DialogRow> = response["rate_limits"]["limits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|limit| {
            let used = limit["percent"].as_f64()?;
            let label = match (limit["kind"].as_str()?, scope_name(&limit["scope"])) {
                ("session", _) => t!("Session (5 hours)"),
                ("weekly_scoped", Some(scope)) => t!("Weekly ({scope})", scope = scope),
                (kind, _) if kind.starts_with("weekly") => t!("Weekly"),
                (kind, _) => kind.replace('_', " ").into(),
            };
            Some(DialogRow {
                badge: Some(format!("{used:.0}%").into()),
                detail: reset_detail(&limit["resets_at"]),
                ..DialogRow::new(label)
            })
        })
        .collect();
    if limits.is_empty() {
        let windows = [
            ("five_hour", t!("Session (5 hours)")),
            ("seven_day", t!("Weekly")),
            ("seven_day_opus", t!("Weekly (Opus)")),
            ("seven_day_sonnet", t!("Weekly (Sonnet)")),
            ("seven_day_oauth_apps", t!("Weekly (OAuth apps)")),
        ];
        limits = windows
            .into_iter()
            .filter_map(|(key, label)| {
                let window = &response["rate_limits"][key];
                let used = window["utilization"].as_f64()?;
                Some(DialogRow {
                    badge: Some(format!("{used:.0}%").into()),
                    detail: reset_detail(&window["resets_at"]),
                    ..DialogRow::new(label)
                })
            })
            .collect();
    }
    if limits.is_empty() {
        limits.push(DialogRow::new(t!(
            "Plan limits are not available for this login method."
        )));
    }
    let mut sections = vec![DialogSection {
        title: t!("Plan limits"),
        rows: limits,
    }];
    let session = &response["session"];
    if session.is_object() {
        let count = |key: &str| session[key].as_u64().unwrap_or_default();
        sections.push(DialogSection {
            title: t!("Session"),
            rows: [
                (
                    t!("Estimated cost"),
                    dollars(session["total_cost_usd"].as_f64().unwrap_or_default()).into(),
                ),
                (
                    t!("Duration"),
                    duration_text(count("total_duration_ms")).into(),
                ),
                (
                    t!("Code changes"),
                    t!(
                        "{added} lines added, {removed} lines removed",
                        added = count("total_lines_added"),
                        removed = count("total_lines_removed")
                    ),
                ),
            ]
            .map(|(label, detail)| DialogRow {
                detail: Some(detail),
                ..DialogRow::new(label)
            })
            .into(),
        });
        let models: Vec<DialogRow> = session["model_usage"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(model, usage)| {
                let tokens = |key: &str| with_separators(usage[key].as_u64().unwrap_or_default());
                DialogRow {
                    badge: Some(dollars(usage["costUSD"].as_f64().unwrap_or_default()).into()),
                    detail: Some(t!(
                        "{input} in, {output} out, {read} cache read, {write} cache write",
                        input = tokens("inputTokens"),
                        output = tokens("outputTokens"),
                        read = tokens("cacheReadInputTokens"),
                        write = tokens("cacheCreationInputTokens")
                    )),
                    ..DialogRow::new(
                        text(usage, "canonicalModel").unwrap_or_else(|| model.clone().into()),
                    )
                }
            })
            .collect();
        if !models.is_empty() {
            sections.push(DialogSection {
                title: t!("Usage by model"),
                rows: models,
            });
        }
    }
    sections
}

/// Deferred, buffer and free categories are not part of `totalTokens`, so they get a section of their own.
pub(crate) fn context_sections(response: &Value) -> Vec<DialogSection> {
    let capacity = response["rawMaxTokens"]
        .as_u64()
        .or_else(|| response["maxTokens"].as_u64())
        .filter(|capacity| *capacity > 0);
    let row = |label: SharedString, tokens: u64, badge: Option<SharedString>| DialogRow {
        detail: Some(t!("{count} tokens", count = with_separators(tokens))),
        badge: badge.or_else(|| {
            capacity
                .map(|capacity| format!("{:.1}%", tokens as f64 * 100.0 / capacity as f64).into())
        }),
        ..DialogRow::new(label)
    };
    let (counted, other): (Vec<&Value>, Vec<&Value>) = items(response, "categories")
        .partition(|category| category["kind"].as_str().is_none_or(|kind| kind == "used"));
    let category_rows = |categories: Vec<&Value>| -> Vec<DialogRow> {
        categories
            .into_iter()
            .filter_map(|category| {
                Some(row(
                    text(category, "name")?,
                    category["tokens"].as_u64().unwrap_or_default(),
                    None,
                ))
            })
            .collect()
    };
    let memory_files: Vec<DialogRow> = items(response, "memoryFiles")
        .filter_map(|file| {
            let path = file["path"].as_str()?;
            let name = Path::new(path).file_name().map_or_else(
                || path.to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            Some(row(
                name.into(),
                file["tokens"].as_u64().unwrap_or_default(),
                Some(text(file, "type").unwrap_or_default()),
            ))
        })
        .collect();
    [
        (t!("In context"), category_rows(counted)),
        (t!("Reserved and free"), category_rows(other)),
        (t!("Memory files"), memory_files),
    ]
    .into_iter()
    .filter(|(_, rows)| !rows.is_empty())
    .map(|(title, rows)| DialogSection { title, rows })
    .collect()
}

/// `render_form` draws a choice as a button group of a fixed size, so any other option count fails the build.
const fn choice_options(options: &'static [&'static str]) -> &'static [&'static str] {
    assert!(options.len() == 2 || options.len() == 3);
    options
}

const MCP_TRANSPORTS: &[&str] = choice_options(&["http", "sse", "stdio"]);
const MCP_SCOPES: &[&str] = choice_options(&["local", "user", "project"]);
const RULE_BEHAVIORS: &[&str] = choice_options(&["allow", "ask", "deny"]);
const RULE_SOURCES: &[&str] = choice_options(&["userSettings", "localSettings"]);
const PLUGIN_SCOPES: &[&str] = choice_options(&["user", "project", "local"]);
const EDIT_RULES: [&str; 2] = ["edit-permission-rules", "--json"];
const RELOAD_ATTEMPTS: usize = 8;
const RELOAD_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);

pub(crate) fn mcp_sections(response: &Value, live_run: bool) -> Vec<DialogSection> {
    let mut sections = Vec::new();
    for server in items(response, "mcpServers") {
        let Some(name) = text(server, "name") else {
            continue;
        };
        let name_text: &str = &name;
        let status = text(server, "status").unwrap_or_default();
        let named = |subtype: &str| json!({ "subtype": subtype, "serverName": name_text });
        let control = |label: SharedString, request: Value| RowAction::Control { label, request };
        let toggle = |label: SharedString, enabled: bool| {
            let mut request = named("mcp_toggle");
            request["enabled"] = json!(enabled);
            control(label, request)
        };
        // Reconnecting and the OAuth callback need a CLI process that outlives the request, which a one-off process doesn't.
        let reconnect = || control(t!("Reconnect"), named("mcp_reconnect"));
        let mut actions = match status.as_ref() {
            "connected" => {
                let mut actions = vec![toggle(t!("Disable"), false)];
                actions.extend(live_run.then(reconnect));
                if matches!(server["config"]["type"].as_str(), Some("http" | "sse")) {
                    actions.push(control(t!("Clear authentication"), named("mcp_clear_auth")));
                }
                actions
            }
            "disabled" => vec![toggle(t!("Enable"), true)],
            "pending" => vec![toggle(t!("Disable"), false)],
            "failed" => live_run.then(reconnect).into_iter().collect(),
            "needs-auth" => live_run
                .then(|| RowAction::Authenticate {
                    label: t!("Authenticate"),
                    request: named("mcp_authenticate"),
                })
                .into_iter()
                .collect(),
            _ => Vec::new(),
        };
        let scope = text(server, "scope");
        if let Some(scope) = scope
            .as_ref()
            .filter(|scope| MCP_SCOPES.contains(&scope.as_ref()))
        {
            actions.push(RowAction::Shell {
                label: t!("Remove"),
                arguments: ["mcp", "remove", "--scope", scope, "--", name_text]
                    .map(String::from)
                    .into(),
                input: None,
                confirmation: t!(
                    "Removed {name} from {scope} config. Running sessions keep it until restarted.",
                    name = name_text,
                    scope = scope
                ),
            });
        }
        let tool_count = items(server, "tools").count();
        let row = DialogRow {
            detail: text(server, "error")
                .or_else(|| (tool_count > 0).then(|| t!("{count} tools", count = tool_count))),
            badge: Some(status.clone()),
            actions,
            ..DialogRow::new(name.clone())
        };
        push_grouped(&mut sections, scope.unwrap_or_else(|| t!("Servers")), row);
    }
    sections
}

fn plugin_action(label: SharedString, arguments: &[&str], confirmation: SharedString) -> RowAction {
    RowAction::Shell {
        label,
        arguments: arguments
            .iter()
            .map(|argument| argument.to_string())
            .collect(),
        input: None,
        confirmation,
    }
}

pub(crate) fn plugin_rows(list_json: &Value) -> Vec<DialogRow> {
    list_json
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|plugin| {
            let id = text(plugin, "id")?;
            let id_text: &str = &id;
            let scope = text(plugin, "scope");
            let enabled = plugin["enabled"] == true;
            let mut actions = Vec::new();
            // A managed plugin is set by an administrator, so it can't be switched here.
            if scope.as_deref() != Some("managed") {
                actions.push(if enabled {
                    plugin_action(
                        t!("Disable"),
                        &["plugin", "disable", id_text],
                        t!("Disabled {id}.", id = id_text),
                    )
                } else {
                    plugin_action(
                        t!("Enable"),
                        &["plugin", "enable", id_text],
                        t!("Enabled {id}.", id = id_text),
                    )
                });
            }
            // Built-in and skills-directory plugins have no marketplace install to remove.
            if let Some(scope) = scope
                .as_deref()
                .filter(|scope| PLUGIN_SCOPES.contains(scope))
                && !id_text.ends_with("@builtin")
                && !id_text.ends_with("@skills-dir")
            {
                actions.push(plugin_action(
                    t!("Uninstall"),
                    &["plugin", "uninstall", id_text, "--scope", scope],
                    t!("Uninstalled {id}.", id = id_text),
                ));
            }
            let detail: Vec<String> = [
                text(plugin, "version").filter(|version| version != "unknown"),
                scope,
            ]
            .into_iter()
            .flatten()
            .map(|part| part.to_string())
            .collect();
            Some(DialogRow {
                detail: (!detail.is_empty()).then(|| detail.join(" · ").into()),
                badge: (!enabled).then(|| t!("Disabled")),
                actions,
                ..DialogRow::new(id)
            })
        })
        .collect()
}

pub(crate) fn marketplace_rows(list_json: &Value) -> Vec<DialogRow> {
    list_json
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|marketplace| {
            let name = text(marketplace, "name")?;
            let name_text: &str = &name;
            Some(DialogRow {
                detail: ["repo", "url", "path", "source"]
                    .into_iter()
                    .find_map(|key| text(marketplace, key)),
                actions: vec![
                    plugin_action(
                        t!("Refresh"),
                        &["plugin", "marketplace", "update", name_text],
                        t!("Updated {name}.", name = name_text),
                    ),
                    plugin_action(
                        t!("Remove"),
                        &["plugin", "marketplace", "remove", name_text],
                        t!("Removed marketplace {name}.", name = name_text),
                    ),
                ],
                ..DialogRow::new(name)
            })
        })
        .collect()
}

fn install_plugin_arguments(scope: &str, id: &str) -> Vec<String> {
    ["plugin", "install", id, "--scope", scope]
        .map(String::from)
        .into()
}

/// The CLI would read a leading dash as an option of its own.
fn plugin_input_error(value: &str, empty: SharedString) -> Option<SharedString> {
    if value.is_empty() {
        Some(empty)
    } else if value.starts_with('-') {
        Some(t!("The value can't start with a dash."))
    } else {
        None
    }
}

fn add_server_arguments(transport: &str, scope: &str, name: &str, target: &str) -> Vec<String> {
    let mut arguments: Vec<String> = [
        "mcp",
        "add",
        "--scope",
        scope,
        "--transport",
        transport,
        "--",
        name,
    ]
    .map(String::from)
    .into();
    if transport == "stdio" {
        // shortcut: a stdio command is split on whitespace, so quoted arguments are not supported; upgrade when a server needs one.
        arguments.extend(target.split_whitespace().map(String::from));
    } else {
        arguments.push(target.to_string());
    }
    arguments
}

/// The name stays a plain word because the CLI would read a leading dash in it as an option of its own.
fn server_input_error(name: &str, target: &str) -> Option<SharedString> {
    if name.is_empty() {
        Some(t!("Enter a server name."))
    } else if target.is_empty() {
        Some(t!("Enter a URL or a command."))
    } else if name.starts_with('-') {
        Some(t!("The name can't start with a dash."))
    } else {
        None
    }
}

fn authentication_url(response: &Value) -> Option<String> {
    text(response, "authUrl")
        .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
        .map(|url| url.to_string())
}

fn add_rule_edit(behavior: &str, source: &str, rule: &str) -> Value {
    json!({ "op": "add", "rules": [rule], "behavior": behavior, "destination": source })
}

fn remove_rule_edit(behavior: &str, source: &str, rule: &str) -> Value {
    json!({ "op": "remove", "rule": rule, "behavior": behavior, "source": source })
}

fn permission_row(state: &Value, rule: &Value) -> Option<DialogRow> {
    let text_of_rule = text(rule, "rule")?;
    let (behavior, source) = (rule["behavior"].as_str()?, rule["source"].as_str()?);
    let detail: String = ["prefix", "emphasis", "suffix"]
        .into_iter()
        .filter_map(|part| rule["description"][part].as_str())
        .collect();
    let mut actions = Vec::new();
    if rule["editability"] == "persistent" && rule["notInEffect"] != true {
        match source {
            "userSettings" | "localSettings" => actions.push(RowAction::Shell {
                label: t!("Remove"),
                arguments: EDIT_RULES.map(String::from).into(),
                input: Some(remove_rule_edit(behavior, source, &text_of_rule).to_string()),
                confirmation: t!(
                    "Removed {rule} from the {behavior} rules in {source}.",
                    rule = text_of_rule,
                    behavior = behavior,
                    source = source
                ),
            }),
            "projectSettings" => actions.extend(state["originalCwd"].as_str().map(|directory| {
                RowAction::OpenPath(
                    PathBuf::from(directory)
                        .join(".claude")
                        .join("settings.json"),
                )
            })),
            _ => {}
        }
    }
    Some(DialogRow {
        detail: (!detail.is_empty()).then(|| detail.into()),
        badge: Some(source.to_string().into()),
        actions,
        ..DialogRow::new(text_of_rule)
    })
}

pub(crate) fn permission_sections(response: &Value) -> Vec<DialogSection> {
    let state = &response["state"];
    let mut sections: Vec<DialogSection> = [
        ("allow", t!("Allow")),
        ("ask", t!("Ask")),
        ("deny", t!("Deny")),
    ]
    .into_iter()
    .filter_map(|(behavior, title)| {
        let rows: Vec<DialogRow> = items(state, "rules")
            .filter(|rule| rule["behavior"] == behavior)
            .filter_map(|rule| permission_row(state, rule))
            .collect();
        (!rows.is_empty()).then_some(DialogSection { title, rows })
    })
    .collect();
    let directories: Vec<DialogRow> = items(state, "workspaceDirectories")
        .filter_map(Value::as_str)
        .map(|directory| DialogRow::new(directory.to_string()))
        .collect();
    if !directories.is_empty() {
        sections.push(DialogSection {
            title: t!("Workspace directories"),
            rows: directories,
        });
    }
    sections
}

fn output_style_sections(styles: &Value, settings: &Value, live_run: bool) -> Vec<DialogSection> {
    let current = settings["effective"]["outputStyle"]
        .as_str()
        .unwrap_or("default");
    let note = (!live_run).then(|| {
        DialogRow::new(t!(
            "Start a turn first: the output style applies to the running Claude session."
        ))
    });
    let styles = items(styles, "available_output_styles")
        .filter_map(Value::as_str)
        .map(|style| {
            let mut row = DialogRow::new(style.to_string());
            if style == current {
                row.badge = Some(t!("Current"));
            } else if live_run {
                row.actions.push(RowAction::Control {
                    label: t!("Use"),
                    request: json!({
                        "subtype": "apply_flag_settings",
                        "settings": { "outputStyle": style },
                    }),
                });
            }
            row
        });
    vec![DialogSection {
        title: t!("Styles"),
        rows: note.into_iter().chain(styles).collect(),
    }]
}

type Reload = Box<dyn Fn(&mut App) -> Task<anyhow::Result<Vec<DialogSection>>>>;

#[derive(Clone, Copy, PartialEq)]
enum PluginTab {
    Plugins,
    Marketplaces,
}

impl PluginTab {
    fn form(self) -> FormKind {
        match self {
            PluginTab::Plugins => FormKind::Plugin,
            PluginTab::Marketplaces => FormKind::Marketplace,
        }
    }
}

fn plugins_reload(session: Entity<AgentSession>, tab: PluginTab) -> Reload {
    Box::new(move |cx| {
        let arguments: &[&str] = match tab {
            PluginTab::Plugins => &["plugin", "list", "--json"],
            PluginTab::Marketplaces => &["plugin", "marketplace", "list", "--json"],
        };
        let output = session.update(cx, |session, cx| {
            let arguments = arguments
                .iter()
                .map(|argument| argument.to_string())
                .collect();
            session.run_cli(arguments, None, cx)
        });
        cx.spawn(async move |_| {
            let list: Value = serde_json::from_str(&output.await?)?;
            let (title, rows) = match tab {
                PluginTab::Plugins => (t!("Plugins"), plugin_rows(&list)),
                PluginTab::Marketplaces => (t!("Marketplaces"), marketplace_rows(&list)),
            };
            Ok(if rows.is_empty() {
                Vec::new()
            } else {
                vec![DialogSection { title, rows }]
            })
        })
    })
}

#[derive(Clone, Copy)]
enum FormKind {
    Server,
    Rule,
    Plugin,
    Marketplace,
}

impl FormKind {
    fn add_label(self) -> SharedString {
        match self {
            FormKind::Server => t!("Add server"),
            FormKind::Rule => t!("Add rule"),
            FormKind::Plugin => t!("Install plugin"),
            FormKind::Marketplace => t!("Add marketplace"),
        }
    }
}

enum FormItem {
    Text {
        label: SharedString,
        editor: Entity<Editor>,
    },
    Choice {
        label: SharedString,
        name: &'static str,
        options: &'static [&'static str],
        selected: usize,
    },
}

struct DialogForm {
    kind: FormKind,
    items: Vec<FormItem>,
    error: Option<SharedString>,
}

impl DialogForm {
    fn text(&self, index: usize, cx: &App) -> String {
        match self.items.get(index) {
            Some(FormItem::Text { editor, .. }) => editor.read(cx).text(cx).trim().to_string(),
            _ => String::new(),
        }
    }

    fn choice(&self, index: usize) -> &'static str {
        match self.items.get(index) {
            Some(FormItem::Choice {
                options, selected, ..
            }) => options.get(*selected).copied().unwrap_or_default(),
            _ => "",
        }
    }
}

fn choice_item(
    label: SharedString,
    name: &'static str,
    options: &'static [&'static str],
) -> FormItem {
    FormItem::Choice {
        label,
        name,
        options,
        selected: 0,
    }
}

enum DialogState {
    Loading,
    Failed(SharedString),
    Loaded(Vec<DialogSection>),
}

pub(crate) struct ClaudeDialog {
    title: SharedString,
    session: Entity<AgentSession>,
    workspace: WeakEntity<Workspace>,
    reload: Reload,
    state: DialogState,
    add: Option<FormKind>,
    form: Option<DialogForm>,
    confirmation: Option<SharedString>,
    plugin_tab: Option<PluginTab>,
    store: Option<Entity<AgentStore>>,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    _pending: Task<()>,
}

impl EventEmitter<DismissEvent> for ClaudeDialog {}
impl ModalView for ClaudeDialog {}

impl Focusable for ClaudeDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ClaudeDialog {
    fn new(
        title: SharedString,
        session: Entity<AgentSession>,
        workspace: WeakEntity<Workspace>,
        reload: Reload,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut dialog = Self {
            title,
            session,
            workspace,
            reload,
            state: DialogState::Loading,
            add: None,
            form: None,
            confirmation: None,
            plugin_tab: None,
            store: None,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            _pending: Task::ready(()),
        };
        dialog.reload(cx);
        dialog
    }

    fn for_control(
        title: SharedString,
        request: Value,
        sections: fn(&Value) -> Vec<DialogSection>,
        session: Entity<AgentSession>,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let reload: Reload = Box::new({
            let session = session.clone();
            move |cx| {
                let answer = session.update(cx, |session, cx| session.control(request.clone(), cx));
                cx.spawn(async move |_| Ok(sections(&answer.await?)))
            }
        });
        Self::new(title, session, workspace, reload, cx)
    }

    fn output_styles(
        session: Entity<AgentSession>,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let reload: Reload = Box::new({
            let session = session.clone();
            move |cx| {
                let (styles, settings, live_run) = session.update(cx, |session, cx| {
                    (
                        session.control(json!({ "subtype": "reload_output_styles" }), cx),
                        session.control(json!({ "subtype": "get_settings" }), cx),
                        session.has_live_run(),
                    )
                });
                cx.spawn(async move |_| {
                    Ok(output_style_sections(
                        &styles.await?,
                        &settings.await?,
                        live_run,
                    ))
                })
            }
        });
        Self::new(t!("Output Style"), session, workspace, reload, cx)
    }

    fn mcp(
        session: Entity<AgentSession>,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let reload: Reload = Box::new({
            let session = session.clone();
            move |cx| {
                let (status, live_run) = session.update(cx, |session, cx| {
                    (
                        session.control(json!({ "subtype": "mcp_status" }), cx),
                        session.has_live_run(),
                    )
                });
                cx.spawn(async move |_| {
                    let mut sections = mcp_sections(&status.await?, live_run);
                    if !live_run {
                        sections.insert(
                            0,
                            DialogSection {
                                title: t!("Note"),
                                rows: vec![DialogRow::new(t!(
                                    "Start a turn first: reconnecting and authenticating servers needs the running Claude session."
                                ))],
                            },
                        );
                    }
                    Ok(sections)
                })
            }
        });
        let mut dialog = Self::new(t!("MCP servers"), session, workspace, reload, cx);
        dialog.add = Some(FormKind::Server);
        dialog
    }

    fn permissions(
        session: Entity<AgentSession>,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut dialog = Self::for_control(
            t!("Permissions"),
            json!({ "subtype": "list_permission_rules" }),
            permission_sections,
            session,
            workspace,
            cx,
        );
        dialog.add = Some(FormKind::Rule);
        dialog
    }

    fn plugins(
        session: Entity<AgentSession>,
        workspace: WeakEntity<Workspace>,
        store: Entity<AgentStore>,
        cx: &mut Context<Self>,
    ) -> Self {
        let reload = plugins_reload(session.clone(), PluginTab::Plugins);
        let mut dialog = Self::new(t!("Plugins"), session, workspace, reload, cx);
        dialog.plugin_tab = Some(PluginTab::Plugins);
        dialog.add = Some(FormKind::Plugin);
        dialog.store = Some(store);
        dialog
    }

    fn select_tab(&mut self, tab: PluginTab, cx: &mut Context<Self>) {
        if self.plugin_tab == Some(tab) {
            return;
        }
        self.plugin_tab = Some(tab);
        self.reload = plugins_reload(self.session.clone(), tab);
        self.add = Some(tab.form());
        self.confirmation = None;
        self.reload(cx);
    }

    fn open_install_form(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.open_form(FormKind::Plugin, window, cx);
        let editor = match self.form.as_ref().and_then(|form| form.items.first()) {
            Some(FormItem::Text { editor, .. }) => editor.clone(),
            _ => return,
        };
        editor.update(cx, |editor, cx| editor.set_text(id, window, cx));
    }

    /// Plugins changed on disk, so the live run reloads them and the slash menu discovers its commands again.
    fn finish_plugin_change(&mut self, confirmation: SharedString, cx: &mut Context<Self>) {
        let (kind, cwd) = {
            let session = self.session.read(cx);
            (session.kind(), session.metadata().cwd.clone())
        };
        if let Some(store) = &self.store {
            store.update(cx, |store, _| store.invalidate_commands(kind, &cwd));
        }
        let reloaded = self.session.read(cx).has_live_run().then(|| {
            self.session.update(cx, |session, cx| {
                session.control(json!({ "subtype": "reload_plugins" }), cx)
            })
        });
        self._pending = cx.spawn(async move |this, cx| {
            let needs_restart = match reloaded {
                Some(reloaded) => reloaded
                    .await
                    .map_or(true, |response| response["error_count"].as_u64() != Some(0)),
                None => true,
            };
            this.update(cx, |this, cx| {
                this.reload(cx);
                this.confirmation = Some(if needs_restart {
                    format!(
                        "{confirmation} {}",
                        t!("Restart Claude to apply plugin changes")
                    )
                    .into()
                } else {
                    confirmation
                });
            })
            .log_err();
        });
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let sections = (self.reload)(cx);
        self.state = DialogState::Loading;
        self._pending = cx.spawn(async move |this, cx| {
            let sections = sections.await;
            this.update(cx, |this, cx| this.show(sections, cx))
                .log_err();
        });
        cx.notify();
    }

    fn show(&mut self, sections: anyhow::Result<Vec<DialogSection>>, cx: &mut Context<Self>) {
        self.state = match sections {
            Ok(sections) => DialogState::Loaded(sections),
            Err(error) => DialogState::Failed(error.to_string().into()),
        };
        cx.notify();
    }

    /// A running session lists permission rules from its own copy of the settings, which follows the file about a second after it changes.
    fn reload_until_changed(&mut self, before: Option<Vec<DialogSection>>, cx: &mut Context<Self>) {
        self.state = DialogState::Loading;
        self._pending = cx.spawn(async move |this, cx| {
            let sections = async {
                let mut sections = Vec::new();
                for attempt in 0..RELOAD_ATTEMPTS {
                    if attempt > 0 {
                        cx.background_executor().timer(RELOAD_INTERVAL).await;
                    }
                    sections = this.update(cx, |this, cx| (this.reload)(cx))?.await?;
                    if Some(&sections) != before.as_ref() {
                        break;
                    }
                }
                anyhow::Ok(sections)
            }
            .await;
            this.update(cx, |this, cx| this.show(sections, cx))
                .log_err();
        });
        cx.notify();
    }

    fn settle<T: 'static>(
        &mut self,
        task: Task<anyhow::Result<T>>,
        on_success: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        self.state = DialogState::Loading;
        self.confirmation = None;
        self._pending = cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| match result {
                Ok(value) => on_success(this, value, cx),
                Err(error) => {
                    this.state = DialogState::Failed(error.to_string().into());
                    cx.notify();
                }
            })
            .log_err();
        });
        cx.notify();
    }

    fn run_control(&mut self, request: Value, cx: &mut Context<Self>) {
        let answer = self
            .session
            .update(cx, |session, cx| session.control(request, cx));
        self.settle(answer, |this, _, cx| this.reload(cx), cx);
    }

    /// The reloaded list can still be the running agent's stale one, so `confirmation` says what changed.
    fn run_shell(
        &mut self,
        arguments: Vec<String>,
        input: Option<String>,
        confirmation: SharedString,
        cx: &mut Context<Self>,
    ) {
        let before = match &self.state {
            DialogState::Loaded(sections) => Some(sections.clone()),
            _ => None,
        };
        // Only a settings edit (the one that sends input) shows up in a running session's list.
        let wait_for_change = input.is_some() && self.session.read(cx).has_live_run();
        let finished = self
            .session
            .update(cx, |session, cx| session.run_cli(arguments, input, cx));
        self.settle(
            finished,
            move |this, _output, cx| {
                if this.plugin_tab.is_some() {
                    this.finish_plugin_change(confirmation, cx);
                    return;
                }
                this.confirmation = Some(confirmation);
                if wait_for_change {
                    this.reload_until_changed(before, cx);
                } else {
                    this.reload(cx);
                }
            },
            cx,
        );
    }

    fn authenticate(&mut self, request: Value, cx: &mut Context<Self>) {
        let answer = self
            .session
            .update(cx, |session, cx| session.control(request, cx));
        self.settle(
            answer,
            |this, response, cx| match text(&response, "error") {
                Some(error) => {
                    this.state = DialogState::Failed(error);
                    cx.notify();
                }
                None => {
                    if let Some(url) = authentication_url(&response) {
                        cx.open_url(&url);
                    }
                    this.reload(cx);
                }
            },
            cx,
        );
    }

    fn open_form(&mut self, kind: FormKind, window: &mut Window, cx: &mut Context<Self>) {
        let mut text = |label: SharedString| FormItem::Text {
            label,
            editor: cx.new(|cx| Editor::single_line(window, cx)),
        };
        let items = match kind {
            FormKind::Server => vec![
                text(t!("Name")),
                choice_item(t!("Transport"), "claude-mcp-transport", MCP_TRANSPORTS),
                text(t!("URL or command")),
                choice_item(t!("Scope"), "claude-mcp-scope", MCP_SCOPES),
            ],
            FormKind::Rule => vec![
                choice_item(t!("Behavior"), "claude-rule-behavior", RULE_BEHAVIORS),
                text(t!("Rule")),
                choice_item(t!("Save in"), "claude-rule-source", RULE_SOURCES),
            ],
            FormKind::Plugin => vec![
                text(t!("Plugin (name@marketplace)")),
                choice_item(t!("Scope"), "claude-plugin-scope", PLUGIN_SCOPES),
            ],
            FormKind::Marketplace => vec![text(t!("Source (owner/repo, URL or path)"))],
        };
        if let Some(FormItem::Text { editor, .. }) = items
            .iter()
            .find(|item| matches!(item, FormItem::Text { .. }))
        {
            window.focus(&editor.focus_handle(cx), cx);
        }
        self.form = Some(DialogForm {
            kind,
            items,
            error: None,
        });
        cx.notify();
    }

    fn submit_form(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &mut self.form else {
            return;
        };
        let submission = match form.kind {
            FormKind::Server => {
                let (name, target) = (form.text(0, cx), form.text(2, cx));
                match server_input_error(&name, &target) {
                    Some(error) => Err(error),
                    None => {
                        let scope = form.choice(3);
                        Ok((
                            add_server_arguments(form.choice(1), scope, &name, &target),
                            None,
                            t!(
                                "Added {name} to {scope} config. It will be available in new sessions.",
                                name = name,
                                scope = scope
                            ),
                        ))
                    }
                }
            }
            FormKind::Rule => {
                let rule = form.text(1, cx);
                if rule.is_empty() {
                    Err(t!("Enter a rule."))
                } else {
                    let (behavior, source) = (form.choice(0), form.choice(2));
                    Ok((
                        EDIT_RULES.map(String::from).into(),
                        Some(add_rule_edit(behavior, source, &rule).to_string()),
                        t!(
                            "Added {rule} to the {behavior} rules in {source}.",
                            rule = rule,
                            behavior = behavior,
                            source = source
                        ),
                    ))
                }
            }
            FormKind::Plugin => {
                let id = form.text(0, cx);
                match plugin_input_error(&id, t!("Enter a plugin id.")) {
                    Some(error) => Err(error),
                    None => Ok((
                        install_plugin_arguments(form.choice(1), &id),
                        None,
                        t!("Installed {id}.", id = id),
                    )),
                }
            }
            FormKind::Marketplace => {
                let source = form.text(0, cx);
                match plugin_input_error(&source, t!("Enter a marketplace source.")) {
                    Some(error) => Err(error),
                    None => Ok((
                        ["plugin", "marketplace", "add", &source]
                            .map(String::from)
                            .into(),
                        None,
                        t!("Added marketplace {source}.", source = source),
                    )),
                }
            }
        };
        match submission {
            Err(error) => {
                form.error = Some(error);
                cx.notify();
            }
            Ok((arguments, input, confirmation)) => {
                self.form = None;
                self.run_shell(arguments, input, confirmation, cx);
            }
        }
    }

    fn open(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        // The dialog closes first: a modal that loses focus to the prompt or the new tab is dismissed anyway.
        cx.emit(DismissEvent);
        let workspace = self.workspace.clone();
        let create = (!path.exists()).then(|| {
            window.prompt(
                PromptLevel::Info,
                &t!("Create {path}?", path = path.display()),
                None,
                &[
                    PromptButton::new(t!("Create")),
                    PromptButton::cancel(t!("Cancel")),
                ],
                cx,
            )
        });
        window
            .spawn(cx, async move |cx| {
                if let Some(answer) = create
                    && answer.await.ok() != Some(0)
                {
                    return anyhow::Ok(());
                }
                let opened = match create_empty_file(&path, cx).await {
                    Ok(()) => open_path(&workspace, path.clone(), cx).await,
                    Err(error) => Err(error),
                };
                if let Err(error) = opened {
                    let message = t!(
                        "Couldn't open {path}: {error}",
                        path = path.display(),
                        error = error
                    )
                    .to_string();
                    workspace.update(cx, |workspace, cx| {
                        workspace.show_toast(
                            Toast::new(NotificationId::unique::<ClaudeDialog>(), message)
                                .autohide(),
                            cx,
                        )
                    })?;
                }
                Ok(())
            })
            .detach_and_log_err(cx);
    }

    fn render_action(
        &self,
        id: SharedString,
        action: &RowAction,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match action {
            RowAction::OpenPath(path) => {
                let path = path.clone();
                Button::new(id, t!("Open"))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.open(path.clone(), window, cx)),
                    )
                    .into_any_element()
            }
            RowAction::OpenUrl(url) => {
                let url = url.clone();
                Button::new(id, t!("Open"))
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .into_any_element()
            }
            RowAction::Control { label, request } => {
                let request = request.clone();
                Button::new(id, label.clone())
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.run_control(request.clone(), cx)),
                    )
                    .into_any_element()
            }
            RowAction::Toggle {
                on,
                request_on,
                request_off,
            } => {
                let dialog = cx.weak_entity();
                let (request_on, request_off) = (request_on.clone(), request_off.clone());
                Switch::new(id, ToggleState::from(*on))
                    .on_click(move |state, _, cx| {
                        let request = if state.selected() {
                            request_on.clone()
                        } else {
                            request_off.clone()
                        };
                        dialog
                            .update(cx, |dialog, cx| dialog.run_control(request, cx))
                            .log_err();
                    })
                    .into_any_element()
            }
            RowAction::Shell {
                label,
                arguments,
                input,
                confirmation,
            } => {
                let (arguments, input, confirmation) =
                    (arguments.clone(), input.clone(), confirmation.clone());
                Button::new(id, label.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run_shell(arguments.clone(), input.clone(), confirmation.clone(), cx)
                    }))
                    .into_any_element()
            }
            RowAction::Authenticate { label, request } => {
                let request = request.clone();
                Button::new(id, label.clone())
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.authenticate(request.clone(), cx)),
                    )
                    .into_any_element()
            }
        }
    }

    fn choice_group<const OPTIONS: usize>(
        &self,
        name: &'static str,
        options: [&'static str; OPTIONS],
        index: usize,
        selected: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let buttons: [ToggleButtonSimple; OPTIONS] = std::array::from_fn(|position| {
            ToggleButtonSimple::new(
                options[position],
                cx.listener(move |this, _, _, cx| {
                    if let Some(FormItem::Choice { selected, .. }) = this
                        .form
                        .as_mut()
                        .and_then(|form| form.items.get_mut(index))
                    {
                        *selected = position;
                        cx.notify();
                    }
                }),
            )
            .selected(position == selected)
        });
        ToggleButtonGroup::single_row(name, buttons)
            .selected_index(selected)
            .into_any_element()
    }

    fn render_tabs(&self, selected: PluginTab, cx: &mut Context<Self>) -> AnyElement {
        let tab_button = |label: SharedString, tab: PluginTab, cx: &mut Context<Self>| {
            ToggleButtonSimple::new(
                label,
                cx.listener(move |this, _, _, cx| this.select_tab(tab, cx)),
            )
            .selected(selected == tab)
        };
        let buttons = [
            tab_button(t!("Plugins"), PluginTab::Plugins, cx),
            tab_button(t!("Marketplaces"), PluginTab::Marketplaces, cx),
        ];
        ToggleButtonGroup::single_row("claude-plugin-tabs", buttons)
            .selected_index(usize::from(selected == PluginTab::Marketplaces))
            .into_any_element()
    }

    fn render_form(&self, form: &DialogForm, cx: &mut Context<Self>) -> Section {
        let heading = |label: &SharedString| {
            Label::new(label.clone())
                .size(LabelSize::Small)
                .color(Color::Muted)
        };
        let mut fields = v_flex().gap_2();
        for (index, item) in form.items.iter().enumerate() {
            fields = fields.child(match item {
                FormItem::Text { label, editor } => v_flex().gap_1().child(heading(label)).child(
                    div()
                        .w_full()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .border_1()
                        .border_color(cx.theme().colors().border)
                        .child(editor.clone()),
                ),
                FormItem::Choice {
                    label,
                    name,
                    options,
                    selected,
                } => v_flex()
                    .gap_1()
                    .child(heading(label))
                    .child(match **options {
                        [first, second] => {
                            self.choice_group(name, [first, second], index, *selected, cx)
                        }
                        [first, second, third] => {
                            self.choice_group(name, [first, second, third], index, *selected, cx)
                        }
                        _ => div().into_any_element(),
                    }),
            });
        }
        Section::new().child(fields.when_some(form.error.clone(), |this, error| {
            this.child(Label::new(error).color(Color::Error))
        }))
    }

    fn render_row(&self, id: &str, row: &DialogRow, cx: &mut Context<Self>) -> impl IntoElement {
        let actions: Vec<AnyElement> = row
            .actions
            .iter()
            .enumerate()
            .map(|(index, action)| self.render_action(format!("{id}-{index}").into(), action, cx))
            .collect();
        h_flex()
            .w_full()
            .gap_2()
            .py_0p5()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .child(Label::new(row.label.clone()))
                            .when_some(row.badge.clone(), |this, badge| {
                                this.child(
                                    Label::new(badge).size(LabelSize::Small).color(Color::Muted),
                                )
                            }),
                    )
                    .when_some(row.detail.clone(), |this, detail| {
                        this.child(
                            Label::new(detail)
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                                .truncate(),
                        )
                    }),
            )
            .children(actions)
    }
}

async fn create_empty_file(
    path: &std::path::Path,
    cx: &mut gpui::AsyncWindowContext,
) -> anyhow::Result<()> {
    if path.exists() {
        return Ok(());
    }
    let path = path.to_path_buf();
    cx.background_spawn(async move {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match std::fs::File::create_new(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => Err(error.into()),
            _ => Ok(()),
        }
    })
    .await
}

impl Render for ClaudeDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut sections = Vec::new();
        if let Some(form) = &self.form {
            sections.push(self.render_form(form, cx));
        } else {
            match &self.state {
                DialogState::Loading => sections
                    .push(Section::new().child(Label::new(t!("Loading…")).color(Color::Muted))),
                DialogState::Failed(message) => sections
                    .push(Section::new().child(Label::new(message.clone()).color(Color::Error))),
                DialogState::Loaded(loaded) if loaded.is_empty() => sections.push(
                    Section::new().child(Label::new(t!("Nothing to show")).color(Color::Muted)),
                ),
                DialogState::Loaded(loaded) => {
                    for (section_index, section) in loaded.iter().enumerate() {
                        let rows: Vec<AnyElement> = section
                            .rows
                            .iter()
                            .enumerate()
                            .map(|(row_index, row)| {
                                self.render_row(
                                    &format!("claude-dialog-{section_index}-{row_index}"),
                                    row,
                                    cx,
                                )
                                .into_any_element()
                            })
                            .collect();
                        sections.push(
                            Section::new()
                                .header(SectionHeader::new(section.title.clone()))
                                .children(rows),
                        );
                    }
                }
            }
        }
        if let (Some(confirmation), DialogState::Loaded(_)) = (&self.confirmation, &self.state)
            && self.form.is_none()
        {
            sections.insert(
                0,
                Section::new().child(Label::new(confirmation.clone()).color(Color::Success)),
            );
        }
        if let (Some(tab), None) = (self.plugin_tab, &self.form) {
            sections.insert(0, Section::new().child(self.render_tabs(tab, cx)));
        }
        let failed = matches!(self.state, DialogState::Failed(_));
        let can_add = self
            .add
            .filter(|_| matches!(self.state, DialogState::Loaded(_)));
        let mut modal = Modal::new("claude-dialog", Some(self.scroll_handle.clone())).header(
            ModalHeader::new()
                .show_dismiss_button(true)
                .headline(self.title.clone()),
        );
        for section in sections {
            modal = modal.section(section);
        }
        if let Some(form) = &self.form {
            let submit_label = form.kind.add_label();
            modal = modal.footer(
                ModalFooter::new().end_slot(
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new("claude-dialog-cancel-add", t!("Cancel")).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.form = None;
                                    cx.notify();
                                }),
                            ),
                        )
                        .child(
                            Button::new("claude-dialog-submit-add", submit_label)
                                .style(ButtonStyle::Filled)
                                .on_click(cx.listener(|this, _, _, cx| this.submit_form(cx))),
                        ),
                ),
            );
        } else if failed || can_add.is_some() {
            modal = modal.footer(
                ModalFooter::new()
                    .start_slot::<Button>(can_add.map(|kind| {
                        Button::new("claude-dialog-add-server", kind.add_label()).on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.open_form(kind, window, cx)
                            }),
                        )
                    }))
                    .end_slot::<Button>(failed.then(|| {
                        Button::new("claude-dialog-close", t!("Close"))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
                    })),
            );
        }
        v_flex()
            .track_focus(&self.focus_handle)
            .elevation_3(cx)
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(DismissEvent)))
            .occlude()
            .w(rems(34.))
            .max_h(rems(40.))
            .child(modal)
    }
}

enum LoginState {
    Starting,
    Waiting,
    Checking,
    Failed(SharedString),
}

/// Signs in to Claude: the browser opens the sign-in page, and a code from it can be pasted here when the browser can't hand it back.
pub(crate) struct LoginDialog {
    state: LoginState,
    manual_url: Option<String>,
    code: Entity<Editor>,
    codes: tokio::sync::mpsc::Sender<String>,
    focus_handle: FocusHandle,
    _login: Task<()>,
}

impl EventEmitter<DismissEvent> for LoginDialog {}
impl ModalView for LoginDialog {}

impl Focusable for LoginDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl LoginDialog {
    fn new(
        cwd: PathBuf,
        store: Entity<AgentStore>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (urls_sender, urls) = tokio::sync::oneshot::channel();
        let (codes, pasted) = tokio::sync::mpsc::channel(1);
        let login = spawn_agent_work(cx, async move {
            agent_harness::ClaudeHarness::new()
                .login(&cwd, urls_sender, pasted)
                .await
        });
        let login = cx.spawn_in(window, async move |this, cx| {
            if let Ok(urls) = urls.await {
                this.update_in(cx, |this, window, cx| {
                    if let Some(url) = &urls.automatic {
                        cx.open_url(url);
                    }
                    this.manual_url = urls.manual;
                    this.state = LoginState::Waiting;
                    window.focus(&this.code.focus_handle(cx), cx);
                    cx.notify();
                })
                .log_err();
            }
            let signed_in = match login.await {
                Ok(signed_in) => signed_in.map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            this.update(cx, |this, cx| match signed_in {
                Ok(_) => {
                    store.update(cx, |store, cx| store.account_changed(AgentKind::Claude, cx));
                    show_toast(&workspace, t!("Signed in to Claude."), cx);
                    cx.emit(DismissEvent);
                }
                Err(error) => {
                    this.state = LoginState::Failed(error.into());
                    cx.notify();
                }
            })
            .log_err();
        });
        let code = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text(&t!("Paste the code from the sign-in page"), window, cx);
            editor
        });
        Self {
            state: LoginState::Starting,
            manual_url: None,
            code,
            codes,
            focus_handle: cx.focus_handle(),
            _login: login,
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let code = self.code.read(cx).text(cx);
        // The code is a secret, so a refused send is dropped without logging it.
        if !matches!(self.state, LoginState::Waiting)
            || code.trim().is_empty()
            || self.codes.try_send(code).is_err()
        {
            return;
        }
        self.state = LoginState::Checking;
        cx.notify();
    }
}

impl Render for LoginDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let waiting = matches!(self.state, LoginState::Waiting);
        let status = match &self.state {
            LoginState::Starting => Label::new(t!("Opening the sign-in page…")).color(Color::Muted),
            LoginState::Waiting => Label::new(t!(
                "Finish signing in in your browser. If the browser can't return to Wu, paste the code it shows."
            ))
            .color(Color::Muted),
            LoginState::Checking => Label::new(t!("Signing in…")).color(Color::Muted),
            LoginState::Failed(error) => Label::new(error.clone()).color(Color::Error),
        };
        let mut body = Section::new().child(status);
        if waiting {
            if let Some(url) = self.manual_url.clone() {
                body = body.child(
                    h_flex().child(
                        Button::new("claude-login-manual", t!("Open the sign-in page again"))
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
                );
            }
            body = body.child(
                div()
                    .mt_2()
                    .px_2()
                    .py_1()
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .rounded_sm()
                    .child(self.code.clone()),
            );
        }
        let footer = ModalFooter::new().end_slot(
            h_flex()
                .gap_1()
                .child(
                    Button::new("claude-login-cancel", t!("Cancel"))
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                )
                .when(waiting, |this| {
                    this.child(
                        Button::new("claude-login-submit", t!("Sign in"))
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                    )
                }),
        );
        v_flex()
            .track_focus(&self.focus_handle)
            .elevation_3(cx)
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(DismissEvent)))
            .on_action(cx.listener(|this, _: &menu::Confirm, _, cx| this.submit(cx)))
            .occlude()
            .w(rems(34.))
            .child(
                Modal::new("claude-login", None)
                    .header(
                        ModalHeader::new()
                            .show_dismiss_button(true)
                            .headline(t!("Sign in to Claude")),
                    )
                    .section(body)
                    .footer(footer),
            )
    }
}

pub(super) fn show_toast<C: gpui::AppContext>(
    workspace: &WeakEntity<Workspace>,
    message: impl Into<String>,
    cx: &mut C,
) {
    let message = message.into();
    workspace
        .update(cx, |workspace, cx| {
            workspace.show_toast(
                Toast::new(NotificationId::unique::<ClaudeDialog>(), message).autohide(),
                cx,
            )
        })
        .log_err();
}

/// Resolves `file` against the chat's folder and refuses anything that ends up outside it, without following symlinks.
fn export_destination(root: &Path, file: &str) -> anyhow::Result<PathBuf> {
    let mut resolved = PathBuf::new();
    for component in root.join(file).components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            component => resolved.push(component),
        }
    }
    anyhow::ensure!(
        resolved.starts_with(root) && resolved != root,
        t!("{file} is outside the project folder.", file = file).to_string()
    );
    Ok(resolved)
}

/// Never replaces a file: `create_new` fails if the path is taken, even when it was created after a check would have passed.
fn write_new_file(path: &Path, text: &str) -> anyhow::Result<()> {
    use std::io::Write as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => Ok(file.write_all(text.as_bytes())?),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(anyhow::anyhow!(
            t!("{path} already exists.", path = path.display()).to_string()
        )),
        Err(error) => Err(error.into()),
    }
}

fn nth_reply(entries: &[Entry], nth: usize, cx: &App) -> Option<String> {
    entries
        .iter()
        .rev()
        .filter_map(|entry| match entry {
            Entry::Assistant { markdown, .. } => Some(markdown.read(cx).source().to_string()),
            _ => None,
        })
        .nth(nth.saturating_sub(1))
}

impl ChatView {
    pub(super) fn export_conversation(
        &mut self,
        file: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self.session.read(cx).metadata().cwd.clone();
        let destination = match file
            .map(|file| export_destination(&root, &file))
            .transpose()
        {
            Ok(destination) => destination,
            Err(error) => {
                show_toast(&self.workspace, error.to_string(), cx);
                return;
            }
        };
        let answer = self.session.update(cx, |session, cx| {
            session.control(json!({ "subtype": "export_conversation" }), cx)
        });
        let workspace = self.workspace.clone();
        window
            .spawn(cx, async move |cx| {
                let exported = async {
                    let response = answer.await?;
                    let transcript = response["text"].as_str().unwrap_or_default().to_string();
                    anyhow::ensure!(
                        !transcript.is_empty(),
                        t!("Nothing to export yet.").to_string()
                    );
                    // The platform's save dialog asks before replacing a file.
                    let (path, replace) = match destination {
                        Some(path) => (path, false),
                        None => {
                            let name = text(&response, "default_filename");
                            let chosen =
                                cx.update(|_, cx| cx.prompt_for_new_path(&root, name.as_deref()))?;
                            match chosen.await?? {
                                Some(path) => (path, true),
                                None => return Ok(None),
                            }
                        }
                    };
                    let target = path.clone();
                    cx.background_spawn(async move {
                        if replace {
                            Ok(std::fs::write(&target, transcript)?)
                        } else {
                            write_new_file(&target, &transcript)
                        }
                    })
                    .await?;
                    anyhow::Ok(Some(path))
                }
                .await;
                match exported {
                    Ok(Some(path)) => show_toast(
                        &workspace,
                        t!("Exported to {path}.", path = path.display()).to_string(),
                        cx,
                    ),
                    Ok(None) => {}
                    Err(error) => show_toast(
                        &workspace,
                        t!("Couldn't export the conversation: {error}", error = error).to_string(),
                        cx,
                    ),
                }
                anyhow::Ok(())
            })
            .detach_and_log_err(cx);
    }

    pub(super) fn show_login_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.session.read(cx).metadata().cwd.clone();
        let store = self.store.clone();
        let weak_workspace = self.workspace.clone();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.toggle_modal(window, cx, move |window, cx| {
                    LoginDialog::new(cwd, store, weak_workspace, window, cx)
                })
            })
            .log_err();
    }

    pub(super) fn sign_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let answer = window.prompt(
            PromptLevel::Warning,
            &t!("Sign out of Claude?"),
            Some(&t!(
                "Claude Code on this computer stays signed out until you sign in again."
            )),
            &[
                PromptButton::new(t!("Sign out")),
                PromptButton::cancel(t!("Cancel")),
            ],
            cx,
        );
        let session = self.session.clone();
        let store = self.store.clone();
        let workspace = self.workspace.clone();
        cx.spawn(async move |_, cx| {
            if answer.await.ok() != Some(0) {
                return;
            }
            let arguments = vec!["auth".to_string(), "logout".to_string()];
            let signed_out = session
                .update(cx, |session, cx| session.run_cli(arguments, None, cx))
                .await;
            let message = match signed_out {
                Ok(_) => {
                    store.update(cx, |store, cx| store.account_changed(AgentKind::Claude, cx));
                    t!("Signed out of Claude.")
                }
                Err(error) => t!("Couldn't sign out: {error}", error = error),
            };
            show_toast(&workspace, message, cx);
        })
        .detach();
    }

    pub(super) fn copy_reply(&mut self, nth: Option<usize>, cx: &mut Context<Self>) {
        let reply = nth_reply(self.entries(cx), nth.unwrap_or(1), cx);
        let message = match reply {
            Some(reply) => {
                cx.write_to_clipboard(ClipboardItem::new_string(reply));
                t!("Copied the reply to the clipboard.")
            }
            None => t!("There is no reply to copy."),
        };
        show_toast(&self.workspace, message.to_string(), cx);
    }

    /// Opens the plugins dialog, with the install form filled in when `install` names a plugin.
    pub(crate) fn show_plugins_dialog(
        &mut self,
        install: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = self.session.clone();
        let store = self.store.clone();
        let weak_workspace = self.workspace.clone();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.toggle_modal(window, cx, move |window, cx| {
                    let mut dialog = ClaudeDialog::plugins(session, weak_workspace, store, cx);
                    if let Some(id) = install {
                        dialog.open_install_form(id, window, cx);
                    }
                    dialog
                })
            })
            .log_err();
    }

    pub(super) fn show_claude_dialog(
        &mut self,
        command: &AppCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        type Build = Box<
            dyn FnOnce(
                Entity<AgentSession>,
                WeakEntity<Workspace>,
                &mut Context<ClaudeDialog>,
            ) -> ClaudeDialog,
        >;
        let control = |title: SharedString,
                       subtype: &'static str,
                       sections: fn(&Value) -> Vec<DialogSection>|
         -> Build {
            Box::new(move |session, workspace, cx| {
                ClaudeDialog::for_control(
                    title,
                    json!({ "subtype": subtype }),
                    sections,
                    session,
                    workspace,
                    cx,
                )
            })
        };
        let build = match command {
            AppCommand::Status => control(t!("Status"), "get_status", status_sections),
            AppCommand::Hooks => control(t!("Hooks"), "get_hooks_listing", hooks_sections),
            AppCommand::Memory => control(t!("Memory"), "get_memory_dialog", memory_sections),
            AppCommand::Skills => control(t!("Skills"), "get_skills_dialog", skills_sections),
            AppCommand::Sandbox => control(t!("Sandbox"), "get_sandbox_dialog", sandbox_sections),
            AppCommand::OutputStyle => Box::new(ClaudeDialog::output_styles),
            AppCommand::Mcp => Box::new(ClaudeDialog::mcp),
            AppCommand::Permissions => Box::new(ClaudeDialog::permissions),
            AppCommand::Usage => control(t!("Usage"), "get_usage", usage_sections),
            AppCommand::Plugins => return self.show_plugins_dialog(None, window, cx),
            AppCommand::Model
            | AppCommand::New
            | AppCommand::Resume
            | AppCommand::Settings
            | AppCommand::Diff
            | AppCommand::Files
            | AppCommand::Terminal
            | AppCommand::Stop
            | AppCommand::Plan { .. }
            | AppCommand::Fork { .. }
            | AppCommand::Rewind
            | AppCommand::Export { .. }
            | AppCommand::Copy { .. }
            | AppCommand::Btw { .. }
            | AppCommand::Login
            | AppCommand::Logout
            | AppCommand::Bookmarks
            | AppCommand::Bug
            | AppCommand::Config
            | AppCommand::Tasks => return,
        };
        let session = self.session.clone();
        let weak_workspace = self.workspace.clone();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.toggle_modal(window, cx, move |_, cx| build(session, weak_workspace, cx))
            })
            .log_err();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentKind,
        session::tests::{new_store, run_until},
    };
    use gpui::TestAppContext;

    fn paths(row: &DialogRow) -> Vec<PathBuf> {
        row.actions
            .iter()
            .filter_map(|action| match action {
                RowAction::OpenPath(path) => Some(path.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn status_sections_keep_order() {
        let response = json!({
            "sections": [
                {
                    "title": "Session",
                    "rows": [
                        { "label": "Version", "value": "2.1.332" },
                        { "value": "Managed by your IT administrator." }
                    ]
                },
                { "title": "Environment", "rows": [{ "label": "Model", "value": "claude-opus-5-5" }] }
            ]
        });
        let sections = status_sections(&response);
        let titles: Vec<&str> = sections
            .iter()
            .map(|section| section.title.as_ref())
            .collect();
        assert_eq!(titles, ["Session", "Environment"]);
        assert_eq!(sections[0].rows[0].label, "Version");
        assert_eq!(sections[0].rows[0].detail.as_deref(), Some("2.1.332"));
        assert_eq!(
            sections[0].rows[1].label,
            "Managed by your IT administrator."
        );
        assert_eq!(sections[0].rows[1].detail, None);
        assert_eq!(
            sections[1].rows[0].detail.as_deref(),
            Some("claude-opus-5-5")
        );
    }

    #[test]
    fn hooks_sections_group_by_event() {
        let response = json!({
            "hooks": [
                { "event": "PreToolUse", "matcher": "Bash", "source": "userSettings" },
                { "event": "Stop", "matcher": "", "source": "projectSettings" },
                { "event": "PreToolUse", "matcher": "Edit", "source": "pluginHook", "pluginName": "lint@market" }
            ],
            "events": [
                { "name": "PreToolUse", "summary": "Before tool execution" },
                { "name": "Stop", "summary": "Right before Claude concludes its response" }
            ]
        });
        let sections = hooks_sections(&response);
        let titles: Vec<&str> = sections
            .iter()
            .map(|section| section.title.as_ref())
            .collect();
        assert_eq!(
            titles,
            [
                "PreToolUse · Before tool execution",
                "Stop · Right before Claude concludes its response"
            ]
        );
        let labels: Vec<&str> = sections[0]
            .rows
            .iter()
            .map(|row| row.label.as_ref())
            .collect();
        assert_eq!(labels, ["Bash", "Edit"]);
        assert_eq!(sections[0].rows[0].badge.as_deref(), Some("userSettings"));
        assert_eq!(sections[0].rows[1].badge.as_deref(), Some("lint@market"));
        assert_eq!(sections[1].rows[0].label, "*");
    }

    #[test]
    fn memory_sections_list_files_with_open_actions() {
        let mut response = json!({
            "files": [
                { "kind": "user", "path": "/home/me/.claude/CLAUDE.md", "label": "User instructions", "description": "Saved in ~/.claude/CLAUDE.md", "exists": true },
                { "kind": "project", "path": "/repo/CLAUDE.md", "label": "Project instructions", "description": "Checked in at ./CLAUDE.md", "exists": false }
            ],
            "memories": [
                { "name": "MEMORY.md", "path": "/home/me/memory/MEMORY.md", "description": "Index of saved memories" }
            ],
            "auto_memory": { "enabled": true, "toggleable": true, "status": "on" }
        });
        let sections = memory_sections(&response);
        let files = &sections[0].rows;
        assert_eq!(files[0].label, "User instructions");
        assert_eq!(
            paths(&files[0]),
            [PathBuf::from("/home/me/.claude/CLAUDE.md")]
        );
        assert_eq!(files[0].badge, None);
        assert_eq!(paths(&files[1]), [PathBuf::from("/repo/CLAUDE.md")]);
        assert_eq!(files[1].badge, Some(t!("Not created yet")));
        let auto = &sections[1].rows;
        assert_eq!(
            auto[0].actions,
            [RowAction::Toggle {
                on: true,
                request_on: json!({ "subtype": "update_settings", "source": "userSettings", "settings": { "autoMemoryEnabled": true } }),
                request_off: json!({ "subtype": "update_settings", "source": "userSettings", "settings": { "autoMemoryEnabled": false } }),
            }]
        );
        assert_eq!(
            paths(&auto[1]),
            [PathBuf::from("/home/me/memory/MEMORY.md")]
        );

        response["auto_memory"]["toggleable"] = json!(false);
        assert_eq!(memory_sections(&response)[1].rows[0].actions, []);
        assert_eq!(memory_sections(&json!({})), []);
    }

    #[test]
    fn skills_sections_group_by_source() {
        let response = json!({
            "skills": [
                { "name": "review", "display_name": "review", "state": "on", "source": "userSettings", "tokens": 120 },
                { "name": "lint:check", "display_name": "check", "state": "off", "source": "plugin", "tokens": 80 },
                { "name": "notes", "display_name": "notes", "state": "on", "source": "userSettings", "tokens": 40 }
            ]
        });
        let sections = skills_sections(&response);
        let titles: Vec<&str> = sections
            .iter()
            .map(|section| section.title.as_ref())
            .collect();
        assert_eq!(titles, ["userSettings", "plugin"]);
        let labels: Vec<&str> = sections[0]
            .rows
            .iter()
            .map(|row| row.label.as_ref())
            .collect();
        assert_eq!(labels, ["review", "notes"]);
        assert_eq!(sections[1].rows[0].label, "check");
        assert_eq!(sections[1].rows[0].badge.as_deref(), Some("off"));
    }

    #[test]
    fn sandbox_sections_show_state_and_non_empty_rules() {
        let response = json!({
            "supported": false,
            "unsupported_reason": "platform",
            "enabled": false,
            "mode": "disabled",
            "excluded_commands": ["docker"],
            "restrictions": { "fs_allow_write": ["/tmp", "/var/tmp"], "network_allowed_domains": [] }
        });
        let sections = sandbox_sections(&response);
        let state: Vec<(&str, Option<&str>)> = sections[0]
            .rows
            .iter()
            .map(|row| (row.label.as_ref(), row.detail.as_deref()))
            .collect();
        assert_eq!(
            state,
            [
                ("Supported", Some("No (platform)")),
                ("Enabled", Some("No")),
                ("Mode", Some("disabled"))
            ]
        );
        assert_eq!(sections[1].rows[0].label, "docker");
        assert_eq!(sections[2].rows.len(), 1);
        assert_eq!(sections[2].rows[0].label, "fs_allow_write");
        assert_eq!(
            sections[2].rows[0].detail.as_deref(),
            Some("/tmp, /var/tmp")
        );
    }

    #[test]
    fn output_styles_mark_the_current_one() {
        let styles = json!({ "available_output_styles": ["default", "Concise"] });
        let rows = &output_style_sections(
            &styles,
            &json!({ "effective": { "outputStyle": "Concise" } }),
            true,
        )[0]
        .rows;
        assert_eq!(
            rows[0].actions,
            [RowAction::Control {
                label: t!("Use"),
                request: json!({ "subtype": "apply_flag_settings", "settings": { "outputStyle": "default" } }),
            }]
        );
        assert_eq!(rows[1].badge, Some(t!("Current")));
        assert_eq!(rows[1].actions, []);

        let rows = &output_style_sections(&styles, &json!({ "effective": {} }), true)[0].rows;
        assert_eq!(rows[0].badge, Some(t!("Current")));
    }

    #[test]
    fn output_styles_without_a_live_run_explain_instead_of_offering_use() {
        let styles = json!({ "available_output_styles": ["default", "Concise"] });
        let rows = &output_style_sections(&styles, &json!({ "effective": {} }), false)[0].rows;
        assert_eq!(
            rows[0].label,
            t!("Start a turn first: the output style applies to the running Claude session.")
        );
        let labels: Vec<&str> = rows[1..].iter().map(|row| row.label.as_ref()).collect();
        assert_eq!(labels, ["default", "Concise"]);
        assert!(rows.iter().all(|row| row.actions.is_empty()));
    }

    fn action_labels(row: &DialogRow) -> Vec<SharedString> {
        row.actions
            .iter()
            .map(|action| match action {
                RowAction::Control { label, .. }
                | RowAction::Authenticate { label, .. }
                | RowAction::Shell { label, .. } => label.clone(),
                other => panic!("unexpected action {other:?}"),
            })
            .collect()
    }

    fn mcp_row(response: &Value, name: &str) -> DialogRow {
        mcp_row_in(response, name, true)
    }

    fn mcp_row_in(response: &Value, name: &str, live_run: bool) -> DialogRow {
        mcp_sections(response, live_run)
            .into_iter()
            .flat_map(|section| section.rows)
            .find(|row| row.label == name)
            .expect("the server is listed")
    }

    #[test]
    fn mcp_rows_offer_actions_by_status() {
        let response = json!({
            "mcpServers": [
                { "name": "files", "status": "connected", "scope": "user", "config": { "type": "stdio" } },
                { "name": "github", "status": "connected", "scope": "project", "config": { "type": "http" } },
                { "name": "off", "status": "disabled", "scope": "local" },
                { "name": "broken", "status": "failed", "scope": "user", "error": "spawn ENOENT" },
                { "name": "linear", "status": "needs-auth", "scope": "user", "config": { "type": "http" } },
                { "name": "booting", "status": "pending" },
                { "name": "plugin", "status": "connected", "scope": "dynamic", "config": { "type": "http" } },
                { "name": "proxy", "status": "connected", "scope": "user", "config": { "type": "claudeai-proxy" } }
            ]
        });
        let labels = |name: &str| action_labels(&mcp_row(&response, name));
        assert_eq!(
            labels("files"),
            [t!("Disable"), t!("Reconnect"), t!("Remove")]
        );
        assert_eq!(
            labels("github"),
            [
                t!("Disable"),
                t!("Reconnect"),
                t!("Clear authentication"),
                t!("Remove")
            ]
        );
        assert_eq!(labels("off"), [t!("Enable"), t!("Remove")]);
        assert_eq!(labels("broken"), [t!("Reconnect"), t!("Remove")]);
        assert_eq!(
            mcp_row(&response, "broken").detail.as_deref(),
            Some("spawn ENOENT")
        );
        assert_eq!(labels("linear"), [t!("Authenticate"), t!("Remove")]);
        assert_eq!(labels("booting"), [t!("Disable")]);
        assert_eq!(
            labels("plugin"),
            [t!("Disable"), t!("Reconnect"), t!("Clear authentication")]
        );
        assert_eq!(
            labels("proxy"),
            [t!("Disable"), t!("Reconnect"), t!("Remove")]
        );

        let github = mcp_row(&response, "github");
        assert_eq!(
            github.actions[0],
            RowAction::Control {
                label: t!("Disable"),
                request: json!({ "subtype": "mcp_toggle", "serverName": "github", "enabled": false }),
            }
        );
        assert_eq!(
            mcp_row(&response, "off").actions[0],
            RowAction::Control {
                label: t!("Enable"),
                request: json!({ "subtype": "mcp_toggle", "serverName": "off", "enabled": true }),
            }
        );
        assert_eq!(
            mcp_row(&response, "linear").actions[0],
            RowAction::Authenticate {
                label: t!("Authenticate"),
                request: json!({ "subtype": "mcp_authenticate", "serverName": "linear" }),
            }
        );
        assert_eq!(
            github.actions[3],
            RowAction::Shell {
                label: t!("Remove"),
                arguments: ["mcp", "remove", "--scope", "project", "--", "github"]
                    .map(String::from)
                    .into(),
                input: None,
                confirmation: t!(
                    "Removed {name} from {scope} config. Running sessions keep it until restarted.",
                    name = "github",
                    scope = "project"
                ),
            }
        );
    }

    #[test]
    fn mcp_rows_without_a_live_run_hide_reconnect_and_authenticate() {
        let response = json!({
            "mcpServers": [
                { "name": "github", "status": "connected", "scope": "project", "config": { "type": "http" } },
                { "name": "broken", "status": "failed", "scope": "user" },
                { "name": "linear", "status": "needs-auth", "scope": "user" },
                { "name": "off", "status": "disabled", "scope": "user" }
            ]
        });
        let labels = |name: &str| action_labels(&mcp_row_in(&response, name, false));
        assert_eq!(
            labels("github"),
            [t!("Disable"), t!("Clear authentication"), t!("Remove")]
        );
        assert_eq!(labels("broken"), [t!("Remove")]);
        assert_eq!(labels("linear"), [t!("Remove")]);
        assert_eq!(labels("off"), [t!("Enable"), t!("Remove")]);
    }

    #[test]
    fn add_server_builds_the_cli_arguments() {
        assert_eq!(
            add_server_arguments("http", "user", "github", "https://example.com/mcp"),
            [
                "mcp",
                "add",
                "--scope",
                "user",
                "--transport",
                "http",
                "--",
                "github",
                "https://example.com/mcp"
            ]
        );
        assert_eq!(
            add_server_arguments("stdio", "local", "notes", "npx -y  some-pkg"),
            [
                "mcp",
                "add",
                "--scope",
                "local",
                "--transport",
                "stdio",
                "--",
                "notes",
                "npx",
                "-y",
                "some-pkg"
            ]
        );
    }

    #[test]
    fn server_names_are_passed_as_single_arguments() {
        let name = "my server; rm -rf /";
        let add = add_server_arguments("stdio", "local", name, "npx");
        assert_eq!(add.len(), 9);
        assert_eq!(add[7], name);
        let response =
            json!({ "mcpServers": [{ "name": name, "status": "disabled", "scope": "local" }] });
        let Some(RowAction::Shell { arguments, .. }) = mcp_row(&response, name).actions.pop()
        else {
            panic!("the row offers no removal");
        };
        assert_eq!(arguments, ["mcp", "remove", "--scope", "local", "--", name]);
    }

    #[test]
    fn add_server_input_is_validated() {
        assert!(server_input_error("github", "https://example.com/mcp").is_none());
        assert!(server_input_error("", "https://example.com/mcp").is_some());
        assert!(server_input_error("github", "").is_some());
        assert!(server_input_error("--scope", "https://example.com/mcp").is_some());
        assert!(server_input_error("github", "--help").is_none());
    }

    #[test]
    fn only_web_addresses_are_opened_for_authentication() {
        let url = |value: Value| authentication_url(&json!({ "authUrl": value }));
        assert_eq!(
            url(json!("https://auth.example.com/start")).as_deref(),
            Some("https://auth.example.com/start")
        );
        assert_eq!(url(json!("file:///etc/passwd")), None);
        assert_eq!(url(json!("javascript:alert(1)")), None);
        assert_eq!(url(json!(null)), None);
    }

    fn permission_response() -> Value {
        json!({
            "state": {
                "rules": [
                    { "behavior": "allow", "source": "userSettings", "rule": "Bash(ls)", "description": { "prefix": "The Bash command ", "emphasis": "ls" }, "editability": "persistent" },
                    { "behavior": "deny", "source": "projectSettings", "rule": "Read(.env)", "editability": "persistent" },
                    { "behavior": "allow", "source": "localSettings", "rule": "Bash(git status)", "description": { "prefix": "The Bash command ", "emphasis": "git status", "suffix": " and its arguments" }, "editability": "persistent" },
                    { "behavior": "ask", "source": "policySettings", "rule": "WebFetch", "editability": "readonly" },
                    { "behavior": "allow", "source": "session", "rule": "Edit", "editability": "session" },
                    { "behavior": "allow", "source": "localSettings", "rule": "Bash(shadowed)", "editability": "persistent", "notInEffect": true }
                ],
                "workspaceDirectories": ["/repo", "/other"],
                "originalCwd": "/repo"
            }
        })
    }

    fn permission_row_in(label: &str) -> DialogRow {
        permission_sections(&permission_response())
            .into_iter()
            .flat_map(|section| section.rows)
            .find(|row| row.label == label)
            .expect("the rule is listed")
    }

    #[test]
    fn permission_rows_are_grouped_by_behavior() {
        let sections = permission_sections(&permission_response());
        let titles: Vec<&str> = sections
            .iter()
            .map(|section| section.title.as_ref())
            .collect();
        assert_eq!(titles, ["Allow", "Ask", "Deny", "Workspace directories"]);
        let labels: Vec<&str> = sections[0]
            .rows
            .iter()
            .map(|row| row.label.as_ref())
            .collect();
        assert_eq!(
            labels,
            ["Bash(ls)", "Bash(git status)", "Edit", "Bash(shadowed)"]
        );
        let row = permission_row_in("Bash(git status)");
        assert_eq!(
            row.detail.as_deref(),
            Some("The Bash command git status and its arguments")
        );
        assert_eq!(row.badge.as_deref(), Some("localSettings"));
        assert_eq!(permission_row_in("Read(.env)").detail, None);
        assert_eq!(sections[3].rows[1].label, "/other");
        assert_eq!(permission_sections(&json!({})), []);
    }

    #[test]
    fn removing_a_rule_sends_one_exact_edit() {
        let row = permission_row_in("Bash(git status)");
        assert_eq!(
            row.actions,
            [RowAction::Shell {
                label: t!("Remove"),
                arguments: ["edit-permission-rules", "--json"].map(String::from).into(),
                input: Some(
                    json!({ "op": "remove", "rule": "Bash(git status)", "behavior": "allow", "source": "localSettings" })
                        .to_string()
                ),
                confirmation: t!(
                    "Removed {rule} from the {behavior} rules in {source}.",
                    rule = "Bash(git status)",
                    behavior = "allow",
                    source = "localSettings"
                ),
            }]
        );
    }

    #[test]
    fn adding_a_rule_sends_one_exact_edit() {
        assert_eq!(
            add_rule_edit("ask", "userSettings", "Bash(npm test)"),
            json!({ "op": "add", "rules": ["Bash(npm test)"], "behavior": "ask", "destination": "userSettings" })
        );
    }

    #[test]
    fn managed_session_and_inactive_rules_have_no_remove_action() {
        for label in ["WebFetch", "Edit", "Bash(shadowed)"] {
            assert_eq!(permission_row_in(label).actions, [], "{label}");
        }
        assert_eq!(
            permission_row_in("Read(.env)").actions,
            [RowAction::OpenPath(PathBuf::from(
                "/repo/.claude/settings.json"
            ))]
        );
    }

    fn shell_arguments(action: &RowAction) -> Vec<&str> {
        match action {
            RowAction::Shell { arguments, .. } => arguments.iter().map(String::as_str).collect(),
            other => panic!("unexpected action {other:?}"),
        }
    }

    #[test]
    fn plugin_rows_parse_the_recorded_list() {
        let recorded: Value =
            serde_json::from_str(include_str!("fixtures/plugin-list.json")).expect("valid JSON");
        let rows = plugin_rows(&recorded);
        assert_eq!(rows.len(), recorded.as_array().map_or(0, Vec::len));
        assert_eq!(rows[0].label, "code-simplifier@claude-plugins-official");
        assert_eq!(rows[0].badge, None);
        assert_eq!(action_labels(&rows[0]), [t!("Disable"), t!("Uninstall")]);
        assert_eq!(
            shell_arguments(&rows[0].actions[0]),
            [
                "plugin",
                "disable",
                "code-simplifier@claude-plugins-official"
            ]
        );
        assert_eq!(
            shell_arguments(&rows[0].actions[1]),
            [
                "plugin",
                "uninstall",
                "code-simplifier@claude-plugins-official",
                "--scope",
                "user"
            ]
        );
        assert_eq!(rows[0].detail.as_deref(), Some("1.0.0 · user"));

        let row = |id: &str| {
            rows.iter()
                .find(|row| row.label == id)
                .expect("the plugin is listed")
        };
        let disabled = row("cc-plugin-telemetry@builtin");
        assert_eq!(disabled.badge, Some(t!("Disabled")));
        assert_eq!(action_labels(disabled), [t!("Enable")]);
        assert_eq!(
            shell_arguments(&disabled.actions[0]),
            ["plugin", "enable", "cc-plugin-telemetry@builtin"]
        );
        assert_eq!(row("cc-plugin-sec-default@builtin").actions, []);
        assert_eq!(
            action_labels(row("review-loop@skills-dir")),
            [t!("Disable")]
        );
    }

    #[test]
    fn marketplace_rows_refresh_and_remove_by_name() {
        let rows = marketplace_rows(&json!([
            { "name": "humanizer", "source": "github", "repo": "blader/humanizer" },
            { "name": "local", "source": "directory", "path": "/work/market" }
        ]));
        assert_eq!(rows[0].detail.as_deref(), Some("blader/humanizer"));
        assert_eq!(rows[1].detail.as_deref(), Some("/work/market"));
        assert_eq!(action_labels(&rows[0]), [t!("Refresh"), t!("Remove")]);
        assert_eq!(
            shell_arguments(&rows[0].actions[0]),
            ["plugin", "marketplace", "update", "humanizer"]
        );
        assert_eq!(
            shell_arguments(&rows[0].actions[1]),
            ["plugin", "marketplace", "remove", "humanizer"]
        );
        assert_eq!(marketplace_rows(&json!({})), []);
    }

    #[test]
    fn install_builds_arguments_with_scope() {
        assert_eq!(
            install_plugin_arguments("project", "ponytail@ponytail"),
            [
                "plugin",
                "install",
                "ponytail@ponytail",
                "--scope",
                "project"
            ]
        );
        assert!(plugin_input_error("ponytail@ponytail", "empty".into()).is_none());
        assert!(plugin_input_error("", "empty".into()).is_some());
        assert!(plugin_input_error("--scope", "empty".into()).is_some());
    }

    #[test]
    fn usage_rows_show_percent_and_reset() {
        let response = json!({
            "rate_limits": {
                "five_hour": { "utilization": 7, "resets_at": "2026-11-26T11:39:59.841243+00:00" },
                "seven_day": { "utilization": 45.4, "resets_at": "2026-12-01T00:59:59+00:00" },
                "seven_day_opus": null
            },
            "session": {
                "total_cost_usd": 0.0042,
                "total_duration_ms": 3_725_000,
                "total_lines_added": 12,
                "total_lines_removed": 3,
                "model_usage": {
                    "claude-opus-5-5": {
                        "inputTokens": 1200, "outputTokens": 340, "cacheReadInputTokens": 50000,
                        "cacheCreationInputTokens": 0, "costUSD": 0.0042
                    }
                }
            }
        });
        let now = "2026-11-26T09:30:00+00:00"
            .parse()
            .expect("a valid timestamp");
        let sections = usage_sections_at(&response, now);
        let limits = &sections[0].rows;
        assert_eq!(limits.len(), 2);
        assert_eq!(limits[0].label, "Session (5 hours)");
        assert_eq!(limits[0].badge.as_deref(), Some("7%"));
        assert_eq!(limits[0].detail.as_deref(), Some("Resets in 2h 9m"));
        assert_eq!(limits[1].badge.as_deref(), Some("45%"));
        assert_eq!(limits[1].detail.as_deref(), Some("Resets in 4d 15h"));
        let session: Vec<(&str, &str)> = sections[1]
            .rows
            .iter()
            .map(|row| {
                (
                    row.label.as_ref(),
                    row.detail.as_deref().unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            session,
            [
                ("Estimated cost", "$0.0042"),
                ("Duration", "1h 2m 5s"),
                ("Code changes", "12 lines added, 3 lines removed")
            ]
        );
        let model = &sections[2].rows[0];
        assert_eq!(model.label, "claude-opus-5-5");
        assert_eq!(model.badge.as_deref(), Some("$0.0042"));
        assert_eq!(
            model.detail.as_deref(),
            Some("1,200 in, 340 out, 50,000 cache read, 0 cache write")
        );

        let unavailable = usage_sections_at(&json!({ "rate_limits": null }), now);
        assert_eq!(unavailable.len(), 1);
        assert_eq!(unavailable[0].rows.len(), 1);
    }

    #[test]
    fn usage_rows_come_from_limits_including_model_scoped_ones() {
        let response = json!({
            "rate_limits": {
                "five_hour": null,
                "limits": [
                    { "kind": "session", "percent": 19, "resets_at": "2026-11-26T11:39:59+00:00", "scope": null },
                    { "kind": "weekly_all", "percent": 47, "resets_at": "2026-12-01T00:59:59+00:00", "scope": null },
                    {
                        "kind": "weekly_scoped", "percent": 19, "resets_at": "2026-12-01T00:59:59+00:00",
                        "scope": { "model": { "id": null, "display_name": "Fable" }, "surface": null }
                    }
                ]
            }
        });
        let now = "2026-11-26T09:30:00+00:00"
            .parse()
            .expect("a valid timestamp");
        let sections = usage_sections_at(&response, now);
        let rows: Vec<(&str, &str)> = sections[0]
            .rows
            .iter()
            .map(|row| (row.label.as_ref(), row.badge.as_deref().unwrap_or_default()))
            .collect();
        assert_eq!(
            rows,
            [
                ("Session (5 hours)", "19%"),
                ("Weekly", "47%"),
                ("Weekly (Fable)", "19%")
            ]
        );
        assert_eq!(
            sections[0].rows[0].detail.as_deref(),
            Some("Resets in 2h 9m")
        );
    }

    #[test]
    fn context_rows_sum_to_total() {
        let response = json!({
            "totalTokens": 3000,
            "rawMaxTokens": 100_000,
            "categories": [
                { "name": "System prompt", "tokens": 2000, "kind": "used" },
                { "name": "Messages", "tokens": 1000, "kind": "used" },
                { "name": "Autocompact buffer", "tokens": 33000, "kind": "buffer" },
                { "name": "Free space", "tokens": 64000, "kind": "free" }
            ],
            "memoryFiles": [
                { "path": "/home/me/.claude/CLAUDE.md", "type": "User", "tokens": 700 }
            ]
        });
        let sections = context_sections(&response);
        let titles: Vec<&str> = sections
            .iter()
            .map(|section| section.title.as_ref())
            .collect();
        assert_eq!(titles, ["In context", "Reserved and free", "Memory files"]);
        let tokens = |row: &DialogRow| -> u64 {
            row.detail
                .as_deref()
                .and_then(|detail| detail.split_whitespace().next())
                .and_then(|count| count.replace(',', "").parse().ok())
                .expect("a token count")
        };
        assert_eq!(sections[0].rows.iter().map(tokens).sum::<u64>(), 3000);
        assert_eq!(sections[0].rows[0].badge.as_deref(), Some("2.0%"));
        assert_eq!(sections[1].rows[1].badge.as_deref(), Some("64.0%"));
        assert_eq!(sections[2].rows[0].label, "CLAUDE.md");
        assert_eq!(sections[2].rows[0].badge.as_deref(), Some("User"));
        assert_eq!(context_sections(&json!({})), []);
    }

    #[test]
    fn export_writes_the_returned_text() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path();
        let path = export_destination(root, "notes/chat.md").expect("inside the project");
        assert_eq!(path, root.join("notes").join("chat.md"));
        write_new_file(&path, "# Chat\n").expect("writes");
        assert_eq!(
            std::fs::read_to_string(&path).expect("exported file"),
            "# Chat\n"
        );
        let error = write_new_file(&path, "other").expect_err("never replaces a file");
        assert!(error.to_string().contains("already exists"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&path).expect("exported file"),
            "# Chat\n"
        );
        assert_eq!(
            export_destination(root, "./a/../b.md").expect("inside the project"),
            root.join("b.md")
        );
    }

    #[test]
    fn export_refuses_paths_outside_the_project() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path();
        let elsewhere = tempfile::tempdir().expect("temporary directory");
        let absolute = elsewhere.path().join("x.md");
        for file in [
            "../x.md",
            "notes/../../x.md",
            absolute.to_str().expect("a UTF-8 path"),
            ".",
            "",
        ] {
            let error = export_destination(root, file).expect_err(file);
            assert!(error.to_string().contains("outside"), "{file}: {error}");
        }
        assert!(export_destination(root, &root.join("x.md").to_string_lossy()).is_ok());
    }

    #[gpui::test]
    fn copy_nth_picks_the_right_reply(cx: &mut TestAppContext) {
        let entries = cx.update(|cx| {
            ["first", "second", "third"]
                .map(|text| Entry::Assistant {
                    markdown: cx.new(|cx| markdown::Markdown::new(text.into(), None, None, cx)),
                    at: 0,
                })
                .into_iter()
                .chain([Entry::Notice {
                    text: "note".into(),
                    is_error: false,
                }])
                .collect::<Vec<_>>()
        });
        cx.update(|cx| {
            assert_eq!(nth_reply(&entries, 1, cx).as_deref(), Some("third"));
            assert_eq!(nth_reply(&entries, 2, cx).as_deref(), Some("second"));
            assert_eq!(nth_reply(&entries, 3, cx).as_deref(), Some("first"));
            assert_eq!(nth_reply(&entries, 4, cx), None);
            assert_eq!(nth_reply(&[], 1, cx), None);
        });
    }

    #[gpui::test]
    async fn control_error_is_shown_not_hung(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let dialog = cx.new(|cx| {
            ClaudeDialog::for_control(
                "Plan".into(),
                json!({ "subtype": "get_plan" }),
                status_sections,
                session,
                WeakEntity::new_invalid(),
                cx,
            )
        });
        let settled = |cx: &mut TestAppContext| {
            dialog.read_with(cx, |dialog, _| {
                !matches!(dialog.state, DialogState::Loading)
            })
        };
        run_until(cx, &settled);
        dialog.update(cx, |dialog, cx| {
            assert!(matches!(dialog.state, DialogState::Loaded(_)));
            assert!(!dialog.session.read(cx).has_live_run());
            dialog.run_control(json!({ "subtype": "mcp_status" }), cx);
        });
        run_until(cx, &settled);
        dialog.read_with(cx, |dialog, _| match &dialog.state {
            DialogState::Failed(message) => {
                assert!(message.contains("MCP servers are unavailable"), "{message}")
            }
            _ => panic!("the CLI's error isn't shown"),
        });
    }
}
