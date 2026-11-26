use std::path::Path;
use std::time::Duration;

use i18n::n;
use serde_json::Value;

use crate::process::Stdio;
use crate::{ClaudeLaunch, ExternalSession, HarnessError};

/// Bounds starting the CLI and each cloud read, so a stalled network ends in an error instead of loading forever.
const READ_TIMEOUT: Duration = Duration::from_secs(60);
const GIT_TIMEOUT: Duration = Duration::from_secs(5);
/// The VS Code extension's limit: a longer transcript is refused rather than read without end.
pub(super) const MAX_TRANSCRIPT_PAGES: usize = 500;
/// What the VS Code extension starts its cloud reader with, so no project MCP server, hook or scheduled task runs for a read.
pub(super) const READER_ENVIRONMENT: [(&str, &str); 3] = [
    ("CLAUDE_CODE_SAFE_MODE", "1"),
    ("CLAUDE_CODE_CONFIG_PROBE", "1"),
    ("CLAUDE_CODE_DISABLE_CRON", "1"),
];
/// The VS Code extension's list of variables that send Claude Code to another provider.
const PROVIDER_VARIABLES: [&str; 7] = [
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    "CLAUDE_CODE_USE_ANTHROPIC_GOOGLE_CLOUD",
    "CLAUDE_CODE_USE_MANTLE",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_GATEWAY",
];

/// A variable as Claude Code gets it: the user's launch settings win over Wu's own environment.
pub(super) fn launch_variable(name: &str) -> Option<String> {
    ClaudeLaunch::current()
        .environment
        .into_iter()
        .rev()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
        .or_else(|| std::env::var(name).ok())
}

fn is_on(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Another provider means no claude.ai sign-in, so there are no cloud chats to show.
pub(super) fn signs_in_elsewhere(variable: impl Fn(&str) -> Option<String>) -> bool {
    PROVIDER_VARIABLES
        .iter()
        .any(|name| variable(name).is_some_and(|value| is_on(&value)))
}

/// Like the VS Code extension, any value turns cloud reads off.
pub(super) fn nonessential_traffic_off(variable: impl Fn(&str) -> Option<String>) -> bool {
    variable("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC").is_some_and(|value| !value.is_empty())
}

pub(super) fn reads_allowed() -> Result<(), HarnessError> {
    if nonessential_traffic_off(launch_variable) {
        return Err(HarnessError::Protocol(
            n!("Web chats are off because CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC is set.").into(),
        ));
    }
    Ok(())
}

pub(super) async fn bounded<T>(
    work: impl Future<Output = Result<T, HarnessError>>,
) -> Result<T, HarnessError> {
    tokio::time::timeout(READ_TIMEOUT, work)
        .await
        .map_err(|_| {
            HarnessError::Protocol(
                n!("Claude Code didn't answer in time. Check your connection and try again.")
                    .into(),
            )
        })?
}

/// The `owner/name` of the GitHub repository behind `origin` in `cwd`, like the VS Code extension reads it.
pub(super) async fn origin_repository(cwd: &Path) -> Option<String> {
    let mut git = crate::process::command("git");
    git.args(["remote", "get-url", "origin"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(GIT_TIMEOUT, git.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    github_repository(String::from_utf8_lossy(&output.stdout).trim())
}

/// A remote URL can carry a token, so only `owner/name` is kept from it.
fn github_repository(url: &str) -> Option<String> {
    let path = url.strip_prefix("git@github.com:").or_else(|| {
        let (_, rest) = url.split_once("://")?;
        let (authority, path) = rest.split_once('/')?;
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        let host = host.split(':').next()?;
        host.eq_ignore_ascii_case("github.com").then_some(path)
    })?;
    let path = path.trim_end_matches('/');
    let (owner, name) = path.strip_suffix(".git").unwrap_or(path).split_once('/')?;
    (!owner.is_empty() && !name.is_empty() && !name.contains('/'))
        .then(|| format!("{owner}/{name}"))
}

/// The CLI's own sentence, written for people, for why it read nothing.
fn refusal(response: &Value) -> Option<HarnessError> {
    if response.get("status").and_then(Value::as_str) == Some("ok") {
        return None;
    }
    let message = response
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or(n!("Claude Code couldn't read your cloud chats."));
    Some(HarnessError::Protocol(message.to_owned()))
}

/// `None` when this sign-in (an API provider or a Console key) or this Claude Code cannot read cloud chats.
pub(super) fn listed_sessions(
    answer: Result<Value, HarnessError>,
    repository: Option<&str>,
) -> Result<Option<Vec<ExternalSession>>, HarnessError> {
    let response = match answer {
        Err(HarnessError::Protocol(message))
            if message.starts_with("Unsupported control request subtype")
                || message.ends_with("is not available on this connection") =>
        {
            return Ok(None);
        }
        answer => answer?,
    };
    let response = &response;
    if matches!(
        response.get("reason").and_then(Value::as_str),
        Some("provider" | "not_signed_in")
    ) {
        return Ok(None);
    }
    if let Some(error) = refusal(response) {
        return Err(error);
    }
    let in_repository = |row: &&Value| {
        let Some(repository) = repository else {
            return true;
        };
        let part = |key: &str| {
            row.pointer(&format!("/repository/{key}"))
                .and_then(Value::as_str)
        };
        matches!((part("owner"), part("name")), (Some(owner), Some(name)) if format!("{owner}/{name}").eq_ignore_ascii_case(repository))
    };
    let mut sessions: Vec<ExternalSession> = response
        .get("sessions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(in_repository)
        .filter_map(|row| {
            Some(ExternalSession {
                id: row.get("id")?.as_str()?.to_owned(),
                title: row
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                updated_at: row
                    .get("updated_at")
                    .and_then(Value::as_str)
                    .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
                    .map_or(0, |time| time.timestamp()),
            })
        })
        .collect();
    sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
    Ok(Some(sessions))
}

/// One page of transcript entries, which have the same shape as the lines of a local transcript, and the cursor of the next page.
pub(super) fn transcript_page(
    mut response: Value,
) -> Result<(Vec<Value>, Option<String>), HarnessError> {
    if let Some(error) = refusal(&response) {
        return Err(error);
    }
    let next_cursor = response
        .get("next_cursor")
        .and_then(Value::as_str)
        .filter(|cursor| !cursor.is_empty())
        .map(str::to_owned);
    match response.get_mut("entries").map(Value::take) {
        Some(Value::Array(entries)) => Ok((entries, next_cursor)),
        _ => Err(HarnessError::Protocol(
            "get_cloud_session_transcript answered without entries".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SESSIONS: &str = include_str!("../../tests/fixtures/claude/cloud-sessions.json");

    #[test]
    fn github_repository_comes_from_any_remote_url_form() {
        for url in [
            "https://github.com/example-owner/example-repo.git",
            "https://someone:secret@github.com/example-owner/example-repo",
            "git@github.com:example-owner/example-repo.git",
            "ssh://git@github.com:22/example-owner/example-repo.git/",
        ] {
            assert_eq!(
                github_repository(url).as_deref(),
                Some("example-owner/example-repo"),
                "{url}"
            );
        }
        for url in [
            "https://gitlab.com/example-owner/example-repo.git",
            "https://github.com/example-owner",
            "/local/path/repo",
        ] {
            assert_eq!(github_repository(url), None, "{url}");
        }
    }

    #[test]
    fn sign_ins_without_cloud_chats_hide_them_and_other_refusals_explain() {
        let sessions: Value = serde_json::from_str(SESSIONS).expect("fixture");
        let listed = listed_sessions(Ok(sessions), None)
            .expect("listed")
            .expect("a claude.ai sign-in");
        assert_eq!(listed.len(), 3, "no repository lists every chat");

        for reason in ["provider", "not_signed_in"] {
            let refused = json!({"status": "unavailable", "reason": reason, "message": "Sign in."});
            assert!(matches!(listed_sessions(Ok(refused), None), Ok(None)));
        }
        let older_cli = HarnessError::Protocol(
            "Unsupported control request subtype: list_cloud_sessions".into(),
        );
        assert!(matches!(listed_sessions(Err(older_cli), None), Ok(None)));
        let stopped = HarnessError::Protocol("claude exited before answering".into());
        assert!(listed_sessions(Err(stopped), None).is_err());
        let busy = json!({"status": "unavailable", "reason": "temporarily_unavailable", "message": "Try again in a moment."});
        assert!(
            matches!(listed_sessions(Ok(busy.clone()), None), Err(HarnessError::Protocol(message)) if message == "Try again in a moment.")
        );
        assert!(transcript_page(busy).is_err());
        assert!(matches!(
            transcript_page(json!({"status": "ok", "entries": [], "next_cursor": ""})),
            Ok((entries, None)) if entries.is_empty()
        ));
    }

    #[test]
    fn cloud_reads_follow_the_launch_environment() {
        let environment = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.to_string())
            }
        };
        assert!(!signs_in_elsewhere(environment(&[])));
        assert!(signs_in_elsewhere(environment(&[(
            "CLAUDE_CODE_USE_BEDROCK",
            "1"
        )])));
        assert!(signs_in_elsewhere(environment(&[(
            "CLAUDE_CODE_USE_GATEWAY",
            "true"
        )])));
        assert!(!signs_in_elsewhere(environment(&[(
            "CLAUDE_CODE_USE_VERTEX",
            "0"
        )])));
        assert!(nonessential_traffic_off(environment(&[(
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
            "1"
        )])));
        assert!(!nonessential_traffic_off(environment(&[(
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
            ""
        )])));
        assert!(!nonessential_traffic_off(environment(&[])));
    }
}
