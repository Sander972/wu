use agent_harness::SlashCommand;
use i18n::{SharedString, t};
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommandToken {
    pub range: Range<usize>,
    pub query: String,
}

fn is_name_char(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '-' | '_' | ':' | '.')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TokenKind {
    Command,
    Mention,
    Skill,
}

impl TokenKind {
    fn prefix(self) -> char {
        match self {
            TokenKind::Command => '/',
            TokenKind::Mention => '@',
            TokenKind::Skill => '$',
        }
    }

    fn accepts(self, character: char) -> bool {
        match self {
            TokenKind::Mention => !character.is_whitespace() && !matches!(character, '@' | '`'),
            TokenKind::Command | TokenKind::Skill => is_name_char(character),
        }
    }
}

#[cfg(test)]
pub(crate) fn command_token(text: &str, cursor: usize) -> Option<CommandToken> {
    token_of_kind(text, cursor, TokenKind::Command)
}

pub(crate) fn completion_token(text: &str, cursor: usize) -> Option<(TokenKind, CommandToken)> {
    [TokenKind::Command, TokenKind::Mention, TokenKind::Skill]
        .into_iter()
        .find_map(|kind| token_of_kind(text, cursor, kind).map(|token| (kind, token)))
}

fn token_of_kind(text: &str, cursor: usize, kind: TokenKind) -> Option<CommandToken> {
    if cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }
    let start = text[..cursor]
        .char_indices()
        .rev()
        .find(|(_, character)| {
            character.is_whitespace() || matches!(character, '(' | '[' | '{' | '>')
        })
        .map_or(0, |(index, character)| index + character.len_utf8());
    let query = text[start..cursor].strip_prefix(kind.prefix())?;
    if !query.chars().all(|character| kind.accepts(character))
        || (kind == TokenKind::Skill && query.starts_with(|character: char| character.is_numeric()))
    {
        return None;
    }
    let end = text[start + 1..]
        .char_indices()
        .find(|(_, character)| !kind.accepts(*character))
        .map_or(text.len(), |(offset, _)| start + 1 + offset);
    // A slash right after the name means a path like /usr/bin.
    if kind == TokenKind::Command && text[end..].starts_with('/') {
        return None;
    }
    Some(CommandToken {
        range: start..end,
        query: query.to_string(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AppCommand {
    Model,
    New,
    Resume,
    Settings,
    Diff,
    Files,
    Terminal,
    Stop,
    Plan { task: Option<String>, open: bool },
    Fork { text: Option<String> },
    Rewind,
    Status,
    Hooks,
    Memory,
    Skills,
    Sandbox,
    OutputStyle,
    Mcp,
    Permissions,
    Plugins,
    Usage,
    Export { file: Option<String> },
    Copy { nth: Option<usize> },
    Btw { question: Option<String> },
    Login,
    Logout,
    Bookmarks,
    Bug,
    Config,
    FocusView,
    Tasks,
    RemoteControl,
    RemoteControlStartup,
}

/// Where Wu opens its issue tracker for `/bug` and `/feedback`.
pub(crate) const ISSUE_URL: &str = "https://github.com/dstrml/wu/issues/new";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommandSection {
    Customize,
    Context,
    Settings,
    Model,
}

impl CommandSection {
    pub fn title(self) -> SharedString {
        match self {
            CommandSection::Customize => t!("Customize"),
            CommandSection::Context => t!("Context"),
            CommandSection::Settings => t!("Settings"),
            CommandSection::Model => t!("Model"),
        }
    }
}

impl AppCommand {
    // Sectioned commands come last and grouped, in the order the menu shows them, so the menu needs no sorting.
    const ALL: [AppCommand; 33] = [
        AppCommand::New,
        AppCommand::Resume,
        AppCommand::Settings,
        AppCommand::Diff,
        AppCommand::Files,
        AppCommand::Terminal,
        AppCommand::Stop,
        AppCommand::Plan {
            task: None,
            open: false,
        },
        AppCommand::Fork { text: None },
        AppCommand::Rewind,
        AppCommand::Copy { nth: None },
        AppCommand::Btw { question: None },
        AppCommand::Login,
        AppCommand::Usage,
        AppCommand::Tasks,
        AppCommand::RemoteControl,
        AppCommand::Mcp,
        AppCommand::Skills,
        AppCommand::OutputStyle,
        AppCommand::Hooks,
        AppCommand::Permissions,
        AppCommand::Memory,
        AppCommand::Status,
        AppCommand::Sandbox,
        AppCommand::Plugins,
        AppCommand::Export { file: None },
        AppCommand::Bookmarks,
        AppCommand::Logout,
        AppCommand::Bug,
        AppCommand::Config,
        AppCommand::FocusView,
        AppCommand::RemoteControlStartup,
        AppCommand::Model,
    ];

    fn section(&self) -> Option<CommandSection> {
        match self {
            AppCommand::Mcp
            | AppCommand::Skills
            | AppCommand::OutputStyle
            | AppCommand::Hooks
            | AppCommand::Permissions
            | AppCommand::Memory
            | AppCommand::Status
            | AppCommand::Sandbox
            | AppCommand::Plugins => Some(CommandSection::Customize),
            AppCommand::Export { .. } | AppCommand::Bookmarks => Some(CommandSection::Context),
            AppCommand::Logout
            | AppCommand::Bug
            | AppCommand::Config
            | AppCommand::FocusView
            | AppCommand::RemoteControlStartup => Some(CommandSection::Settings),
            AppCommand::Model => Some(CommandSection::Model),
            AppCommand::New
            | AppCommand::Resume
            | AppCommand::Settings
            | AppCommand::Diff
            | AppCommand::Files
            | AppCommand::Terminal
            | AppCommand::Stop
            | AppCommand::Plan { .. }
            | AppCommand::Fork { .. }
            | AppCommand::Rewind
            | AppCommand::Usage
            | AppCommand::Copy { .. }
            | AppCommand::Btw { .. }
            | AppCommand::Login
            | AppCommand::Tasks
            | AppCommand::RemoteControl => None,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            AppCommand::Model => "model",
            AppCommand::New => "new",
            AppCommand::Resume => "resume",
            AppCommand::Settings => "settings",
            AppCommand::Diff => "diff",
            AppCommand::Files => "files",
            AppCommand::Terminal => "terminal",
            AppCommand::Stop => "stop",
            AppCommand::Plan { .. } => "plan",
            AppCommand::Fork { .. } => "fork",
            AppCommand::Rewind => "rewind",
            AppCommand::Status => "status",
            AppCommand::Hooks => "hooks",
            AppCommand::Memory => "memory",
            AppCommand::Skills => "skills",
            AppCommand::Sandbox => "sandbox",
            AppCommand::OutputStyle => "output-style",
            AppCommand::Mcp => "mcp",
            AppCommand::Permissions => "permissions",
            AppCommand::Plugins => "plugins",
            AppCommand::Usage => "usage",
            AppCommand::Export { .. } => "export",
            AppCommand::Copy { .. } => "copy",
            AppCommand::Btw { .. } => "btw",
            AppCommand::Login => "login",
            AppCommand::Logout => "logout",
            AppCommand::Bookmarks => "bookmarks",
            AppCommand::Bug => "bug",
            AppCommand::Config => "config",
            AppCommand::FocusView => "focus",
            AppCommand::Tasks => "tasks",
            AppCommand::RemoteControl => "remote-control",
            AppCommand::RemoteControlStartup => "remote-control-startup",
        }
    }

    fn description(&self) -> SharedString {
        match self {
            AppCommand::Model => t!("Wu: choose model and reasoning"),
            AppCommand::New => t!("Wu: start a new chat"),
            AppCommand::Resume => t!("Wu: open chat history"),
            AppCommand::Settings => t!("Wu: open settings"),
            AppCommand::Diff => t!("Wu: open changes"),
            AppCommand::Files => t!("Wu: open project files"),
            AppCommand::Terminal => t!("Wu: open a terminal"),
            AppCommand::Stop => t!("Wu: stop the agent"),
            AppCommand::Plan { .. } => t!("Wu: plan before making changes"),
            AppCommand::Fork { .. } => t!("Wu: fork this chat"),
            AppCommand::Rewind => t!("Wu: rewind code or fork from a message"),
            AppCommand::Status => t!("Wu: show Claude Code status"),
            AppCommand::Hooks => t!("Wu: list hooks"),
            AppCommand::Memory => t!("Wu: open memory files"),
            AppCommand::Skills => t!("Wu: list skills"),
            AppCommand::Sandbox => t!("Wu: show sandbox settings"),
            AppCommand::OutputStyle => t!("Wu: choose an output style"),
            AppCommand::Mcp => t!("Wu: manage MCP servers"),
            AppCommand::Permissions => t!("Wu: manage permission rules"),
            AppCommand::Plugins => t!("Wu: manage plugins and marketplaces"),
            AppCommand::Usage => t!("Wu: show plan usage and session cost"),
            AppCommand::Export { .. } => t!("Wu: save the conversation to a file"),
            AppCommand::Copy { .. } => t!("Wu: copy a reply to the clipboard"),
            AppCommand::Btw { .. } => t!("Wu: ask a side question without interrupting the agent"),
            AppCommand::Login => t!("Wu: sign in to Claude"),
            AppCommand::Logout => t!("Wu: sign out of Claude"),
            AppCommand::Bookmarks => t!("Wu: show saved messages"),
            AppCommand::Bug => t!("Wu: report a problem"),
            AppCommand::Config => t!("Wu: open agent chat settings"),
            AppCommand::FocusView => {
                t!("Wu: hide tool calls and activity, showing only prompts and replies")
            }
            AppCommand::Tasks => t!("Wu: show running background tasks"),
            AppCommand::RemoteControl => {
                t!("Wu: control this chat from claude.ai or the Claude app")
            }
            AppCommand::RemoteControlStartup => {
                t!(
                    "Wu: turn Remote Control for all sessions on or off (a Claude Code setting that Claude Code applies itself; Wu shows its indicator only for chats started with /remote-control)"
                )
            }
        }
    }

    /// Dispatched by name so this crate needn't depend on every panel crate.
    pub fn action_name(&self) -> Option<&'static str> {
        match self {
            AppCommand::Settings => Some("wu::OpenSettings"),
            AppCommand::Diff => Some("git_panel::ToggleFocus"),
            AppCommand::Files => Some("project_panel::ToggleFocus"),
            AppCommand::Terminal => Some("terminal_panel::ToggleFocus"),
            AppCommand::Model
            | AppCommand::New
            | AppCommand::Resume
            | AppCommand::Stop
            | AppCommand::Plan { .. }
            | AppCommand::Fork { .. }
            | AppCommand::Rewind
            | AppCommand::Status
            | AppCommand::Hooks
            | AppCommand::Memory
            | AppCommand::Skills
            | AppCommand::Sandbox
            | AppCommand::OutputStyle
            | AppCommand::Mcp
            | AppCommand::Permissions
            | AppCommand::Plugins
            | AppCommand::Usage
            | AppCommand::Export { .. }
            | AppCommand::Copy { .. }
            | AppCommand::Btw { .. }
            | AppCommand::Login
            | AppCommand::Logout
            | AppCommand::Bookmarks
            | AppCommand::Bug
            | AppCommand::Config
            | AppCommand::FocusView
            | AppCommand::Tasks
            | AppCommand::RemoteControl
            | AppCommand::RemoteControlStartup => None,
        }
    }
}

/// The Wu command that stands in for a CLI built-in Wu handles natively or redirects.
pub(crate) fn hidden_command_target(name: &str) -> Option<AppCommand> {
    match name {
        "clear" => Some(AppCommand::New),
        "model" | "effort" | "fast" => Some(AppCommand::Model),
        "resume" => Some(AppCommand::Resume),
        "bug" | "feedback" => Some(AppCommand::Bug),
        "chrome" => Some(AppCommand::Config),
        "bashes" => Some(AppCommand::Tasks),
        "remote-control" => Some(AppCommand::RemoteControl),
        "remote-control-startup" => Some(AppCommand::RemoteControlStartup),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CommandTarget {
    Agent,
    App(AppCommand),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CommandItem {
    pub name: String,
    pub detail: String,
    pub target: CommandTarget,
    pub is_skill: bool,
    pub section: Option<CommandSection>,
}

pub(crate) fn command_items(agent_commands: &[SlashCommand]) -> Vec<CommandItem> {
    let mut items: Vec<CommandItem> = agent_commands
        .iter()
        .map(|command| CommandItem {
            name: command.name.clone(),
            detail: match (command.description.is_empty(), &command.input_hint) {
                (_, None) => command.description.clone(),
                (true, Some(hint)) => format!("<{hint}>"),
                (false, Some(hint)) => format!("{} · <{hint}>", command.description),
            },
            target: CommandTarget::Agent,
            is_skill: command.is_skill,
            section: None,
        })
        .collect();
    for command in AppCommand::ALL {
        let mut name = command.name().to_string();
        while items.iter().any(|item| item.name == name) {
            name = format!("wu:{name}");
        }
        items.push(CommandItem {
            name,
            detail: command.description().to_string(),
            section: command.section(),
            target: CommandTarget::App(command),
            is_skill: false,
        });
    }
    items
}

pub(crate) fn filter_commands(query: &str, items: Vec<CommandItem>) -> Vec<CommandItem> {
    let query = query.to_lowercase();
    let mut ranked: Vec<(usize, usize, CommandItem)> = items
        .into_iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let name = item.name.to_lowercase();
            let rank = if name.starts_with(&query) {
                0
            } else if name.contains(&query) {
                1
            } else {
                return None;
            };
            Some((rank, index, item))
        })
        .collect();
    ranked.sort_by_key(|(rank, index, _)| (*rank, *index));
    ranked.into_iter().map(|(_, _, item)| item).collect()
}

pub(crate) fn app_command_for_text(text: &str, items: &[CommandItem]) -> Option<AppCommand> {
    let text = text.trim().strip_prefix('/')?;
    let (name, argument) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    let argument = argument.trim();
    items.iter().find_map(|item| match &item.target {
        CommandTarget::App(AppCommand::Plan { .. }) if item.name == name => {
            Some(AppCommand::Plan {
                task: (!argument.is_empty() && argument != "open").then(|| argument.to_string()),
                open: argument == "open",
            })
        }
        CommandTarget::App(AppCommand::Fork { .. }) if item.name == name => {
            Some(AppCommand::Fork {
                text: (!argument.is_empty()).then(|| argument.to_string()),
            })
        }
        CommandTarget::App(AppCommand::Export { .. }) if item.name == name => {
            Some(AppCommand::Export {
                file: (!argument.is_empty()).then(|| argument.to_string()),
            })
        }
        CommandTarget::App(AppCommand::Copy { .. }) if item.name == name => {
            let nth = match argument {
                "" => None,
                number => Some(number.parse::<usize>().ok().filter(|nth| *nth > 0)?),
            };
            Some(AppCommand::Copy { nth })
        }
        CommandTarget::App(AppCommand::Btw { .. }) if item.name == name => Some(AppCommand::Btw {
            question: (!argument.is_empty()).then(|| argument.to_string()),
        }),
        CommandTarget::App(command) if item.name == name && argument.is_empty() => {
            Some(command.clone())
        }
        _ => None,
    })
}

/// What to tell the user when `text` names a Wu command but its arguments don't fit.
pub(crate) fn argument_error(text: &str, items: &[CommandItem]) -> Option<SharedString> {
    let text = text.trim().strip_prefix('/')?;
    let (name, _) = text.split_once(char::is_whitespace)?;
    items.iter().find_map(|item| match &item.target {
        CommandTarget::App(AppCommand::Copy { .. }) if item.name == name => {
            Some(t!("/copy takes a reply number, like /copy 2."))
        }
        CommandTarget::App(_) if item.name == name => {
            Some(t!("/{name} takes no arguments.", name = name))
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(text: &str) -> Option<(Range<usize>, String)> {
        command_token(text, text.len()).map(|token| (token.range, token.query))
    }

    #[test]
    fn tokens_start_at_word_boundaries() {
        assert_eq!(token("/"), Some((0..1, String::new())));
        assert_eq!(token("/rev"), Some((0..4, "rev".into())));
        assert_eq!(token("please /rev"), Some((7..11, "rev".into())));
        assert_eq!(token("(/rev"), Some((1..5, "rev".into())));
        assert_eq!(token("a/rev"), None);
        assert_eq!(token("/rev iew"), None);
        assert_eq!(token("/usr/bin"), None);
        assert_eq!(token("/a+b"), None);
    }

    #[test]
    fn mentions_and_skills_have_their_own_rules() {
        let kind = |text: &str| {
            completion_token(text, text.len()).map(|(kind, token)| (kind, token.query))
        };
        assert_eq!(
            kind("see @src/ma"),
            Some((TokenKind::Mention, "src/ma".into()))
        );
        assert_eq!(kind("mail me@example.com"), None);
        assert_eq!(
            kind("use $review"),
            Some((TokenKind::Skill, "review".into()))
        );
        assert_eq!(kind("costs $5"), None);
        assert_eq!(kind("/comp"), Some((TokenKind::Command, "comp".into())));
    }

    #[test]
    fn token_covers_the_whole_name_around_the_cursor() {
        let text = "/review now";
        assert_eq!(
            command_token(text, 3),
            Some(CommandToken {
                range: 0..7,
                query: "re".into()
            })
        );
        assert_eq!(command_token("/usr/bin", 4), None);
        assert_eq!(command_token("é/x", 1), None);
    }

    fn agent(name: &str) -> SlashCommand {
        SlashCommand {
            name: name.into(),
            description: String::new(),
            input_hint: None,
            is_skill: false,
        }
    }

    #[test]
    fn wu_commands_step_aside_for_agent_commands() {
        let items = command_items(&[agent("review"), agent("model")]);
        let names: Vec<&str> = items.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(&names[..2], ["review", "model"]);
        assert!(names.contains(&"wu:model"));
        assert_eq!(
            app_command_for_text(" /wu:model ", &items),
            Some(AppCommand::Model)
        );
        assert_eq!(app_command_for_text("/model", &items), None);
        assert_eq!(app_command_for_text("/new", &items), Some(AppCommand::New));
        assert_eq!(app_command_for_text("/new chat", &items), None);
    }

    #[test]
    fn plan_takes_a_task_or_open() {
        let items = command_items(&[]);
        let plan = |task: Option<&str>, open| AppCommand::Plan {
            task: task.map(str::to_string),
            open,
        };
        assert_eq!(
            app_command_for_text("/plan", &items),
            Some(plan(None, false))
        );
        assert_eq!(
            app_command_for_text("/plan open", &items),
            Some(plan(None, true))
        );
        assert_eq!(
            app_command_for_text("/plan fix the build", &items),
            Some(plan(Some("fix the build"), false))
        );
    }

    #[test]
    fn fork_takes_text_for_the_new_composer() {
        let items = command_items(&[]);
        assert_eq!(
            app_command_for_text("/fork", &items),
            Some(AppCommand::Fork { text: None })
        );
        assert_eq!(
            app_command_for_text("/fork try again", &items),
            Some(AppCommand::Fork {
                text: Some("try again".into())
            })
        );
        assert_eq!(
            app_command_for_text("/rewind", &items),
            Some(AppCommand::Rewind)
        );
    }

    #[test]
    fn export_takes_a_file_and_copy_a_reply_number() {
        let items = command_items(&[]);
        let parse = |text| app_command_for_text(text, &items);
        assert_eq!(parse("/export"), Some(AppCommand::Export { file: None }));
        assert_eq!(
            parse("/export notes/chat 1.md"),
            Some(AppCommand::Export {
                file: Some("notes/chat 1.md".into())
            })
        );
        assert_eq!(parse("/copy"), Some(AppCommand::Copy { nth: None }));
        assert_eq!(parse("/copy 2"), Some(AppCommand::Copy { nth: Some(2) }));
        assert_eq!(parse("/copy 0"), None);
        assert_eq!(parse("/copy two"), None);
        assert_eq!(parse("/btw"), Some(AppCommand::Btw { question: None }));
        assert_eq!(
            parse("/btw what file?"),
            Some(AppCommand::Btw {
                question: Some("what file?".into())
            })
        );
    }

    #[test]
    fn claude_never_shadows_a_wu_command() {
        let shadowed: Vec<&str> = AppCommand::ALL
            .iter()
            .map(AppCommand::name)
            .filter(|name| !agent_harness::claude::is_hidden_command(name))
            .collect();
        assert_eq!(shadowed, Vec::<&str>::new());
    }

    #[test]
    fn menu_groups_commands_into_sections() {
        let items = command_items(&[agent("review")]);
        let names_in = |section: CommandSection| -> Vec<&str> {
            items
                .iter()
                .filter(|item| item.section == Some(section))
                .map(|item| item.name.as_str())
                .collect()
        };
        assert_eq!(
            names_in(CommandSection::Customize),
            [
                "mcp",
                "skills",
                "output-style",
                "hooks",
                "permissions",
                "memory",
                "status",
                "sandbox",
                "plugins"
            ]
        );
        assert_eq!(names_in(CommandSection::Context), ["export", "bookmarks"]);
        assert_eq!(
            names_in(CommandSection::Settings),
            ["logout", "bug", "config", "focus", "remote-control-startup"]
        );
        assert_eq!(names_in(CommandSection::Model), ["model"]);
        let mut order: Vec<CommandSection> = items.iter().filter_map(|item| item.section).collect();
        order.dedup();
        assert_eq!(
            order,
            [
                CommandSection::Customize,
                CommandSection::Context,
                CommandSection::Settings,
                CommandSection::Model
            ]
        );
        let first_sectioned = items.iter().position(|item| item.section.is_some());
        assert_eq!(
            items.iter().rposition(|item| item.section.is_none()),
            first_sectioned.map(|index| index - 1)
        );
    }

    #[test]
    fn bug_and_feedback_open_the_issue_page() {
        assert_eq!(hidden_command_target("bug"), Some(AppCommand::Bug));
        assert_eq!(hidden_command_target("feedback"), Some(AppCommand::Bug));
        assert_eq!(ISSUE_URL, "https://github.com/dstrml/wu/issues/new");
        assert_eq!(hidden_command_target("chrome"), Some(AppCommand::Config));
        assert_eq!(hidden_command_target("bashes"), Some(AppCommand::Tasks));
        assert_eq!(hidden_command_target("vim"), None);
        assert_eq!(hidden_command_target("config"), None);
        assert_eq!(
            hidden_command_target("remote-control"),
            Some(AppCommand::RemoteControl)
        );
    }

    #[test]
    fn extra_text_after_a_wu_command_gets_an_accurate_message() {
        let items = command_items(&[agent("review")]);
        let message = |text| argument_error(text, &items).map(|message| message.to_string());
        assert_eq!(
            message("/new chat"),
            Some("/new takes no arguments.".into())
        );
        assert_eq!(
            message("/stop now"),
            Some("/stop takes no arguments.".into())
        );
        assert_eq!(
            message("/copy abc"),
            Some("/copy takes a reply number, like /copy 2.".into())
        );
        assert_eq!(message("/new"), None);
        assert_eq!(message("/review now"), None);
        assert_eq!(message("/vim now"), None);
    }

    #[test]
    fn details_show_argument_hints() {
        let command = SlashCommand {
            name: "review".into(),
            description: "Review a pull request".into(),
            input_hint: Some("[pr number]".into()),
            is_skill: false,
        };
        assert_eq!(
            command_items(&[command])[0].detail,
            "Review a pull request · <[pr number]>"
        );
    }

    #[test]
    fn prefix_matches_come_first() {
        let items = command_items(&[agent("compact"), agent("pr-comments"), agent("compress")]);
        let names: Vec<String> = filter_commands("COM", items)
            .into_iter()
            .map(|item| item.name)
            .collect();
        assert_eq!(names, ["compact", "compress", "pr-comments"]);
    }
}
