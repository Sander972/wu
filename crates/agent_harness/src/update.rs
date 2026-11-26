use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::process::Stdio;
use tokio::io::AsyncWriteExt as _;

const UPDATE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CLI_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq)]
pub enum UpdateOutcome {
    Updated(Option<semver::Version>),
    UpToDate,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct UpdateFailed(pub String);

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct CliFailed(pub String);

/// `installed_after` finds the CLI again, since an update can install a new copy that takes over.
pub async fn update_cli(
    executable: PathBuf,
    args: &[&str],
    installed_after: impl FnOnce() -> Option<PathBuf> + Send,
) -> Result<UpdateOutcome, UpdateFailed> {
    let before = version_of(&executable).await;
    let output = run(
        &executable,
        args,
        &crate::executable::scratch_dir(),
        None,
        UPDATE_TIMEOUT,
        i18n::t!("The update took too long.").to_string(),
    )
    .await
    .map_err(UpdateFailed)?;
    if !output.status.success() {
        return Err(UpdateFailed(failure_reason(&output)));
    }
    let after = match installed_after() {
        Some(installed) => version_of(&installed).await,
        None => None,
    };
    Ok(if after.is_some() && after != before {
        UpdateOutcome::Updated(after)
    } else {
        UpdateOutcome::UpToDate
    })
}

/// Runs the CLI with `args` as separate arguments, never through a shell, and returns its stdout, or fails with its stderr when it exits unsuccessfully; `input` is written to its stdin.
pub async fn run_cli(
    executable: &Path,
    args: &[String],
    cwd: &Path,
    input: Option<&str>,
) -> Result<String, CliFailed> {
    let output = run(
        executable,
        args,
        cwd,
        input,
        CLI_COMMAND_TIMEOUT,
        i18n::t!("The command took too long.").to_string(),
    )
    .await
    .map_err(CliFailed)?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(CliFailed(match stderr.trim() {
        "" => failure_reason(&output),
        stderr => stderr.to_owned(),
    }))
}

async fn run(
    executable: &Path,
    args: &[impl AsRef<std::ffi::OsStr>],
    cwd: &Path,
    input: Option<&str>,
    timeout: Duration,
    timed_out: String,
) -> Result<std::process::Output, String> {
    let mut command = crate::process::command(executable);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::compose_child_path(&mut command, executable);
    let exchange = async {
        let mut child = command.spawn()?;
        let written = match (input, child.stdin.take()) {
            (Some(input), Some(mut stdin)) => stdin.write_all(input.as_bytes()).await,
            _ => Ok(()),
        };
        let output = child.wait_with_output().await?;
        // A CLI that refuses early closes its stdin, and its stderr says why where the broken pipe does not.
        match written {
            Err(error) if output.status.success() => Err(error),
            _ => Ok(output),
        }
    };
    tokio::time::timeout(timeout, exchange)
        .await
        .map_err(|_| timed_out)?
        .map_err(|error| error.to_string())
}

async fn version_of(executable: &Path) -> Option<semver::Version> {
    let executable = executable.to_path_buf();
    tokio::task::spawn_blocking(move || crate::executable::binary_version(&executable))
        .await
        .ok()
        .flatten()
}

fn failure_reason(output: &std::process::Output) -> String {
    [&output.stderr, &output.stdout]
        .into_iter()
        .find_map(|bytes| {
            String::from_utf8_lossy(bytes)
                .lines()
                .map(str::trim)
                .rfind(|line| !line.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            let exit = crate::describe_exit(Some(output.status));
            let mut characters = exit.chars();
            characters
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
                .unwrap_or_default()
        })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn fake_cli(dir: &Path, version: &str, update: &str) -> PathBuf {
        let path = dir.join("agent");
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo \"agent {version}\"; exit 0; fi\n{update}\n"
        );
        std::fs::write(&path, script).expect("write fake cli");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("make fake cli runnable");
        path
    }

    #[tokio::test]
    async fn updates_report_the_new_version_or_that_nothing_changed() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let replacement = "#!/bin/sh\\necho agent 2.0.0\\n";
        let cli = fake_cli(
            dir.path(),
            "1.0.0",
            &format!(
                "printf '{replacement}' > \"$0.new\"; chmod +x \"$0.new\"; mv \"$0.new\" \"$0\""
            ),
        );
        let same = || Some(cli.clone());
        assert_eq!(
            update_cli(cli.clone(), &["update"], same)
                .await
                .expect("updates"),
            UpdateOutcome::Updated(Some(semver::Version::new(2, 0, 0)))
        );
        assert_eq!(
            update_cli(cli.clone(), &["update"], same)
                .await
                .expect("updates"),
            UpdateOutcome::UpToDate
        );
    }

    #[tokio::test]
    async fn updates_that_install_a_new_copy_report_its_version() {
        let bundled = tempfile::tempdir().expect("temporary directory");
        let standalone = tempfile::tempdir().expect("temporary directory");
        let installed = fake_cli(standalone.path(), "3.1.0", "exit 0");
        let cli = fake_cli(bundled.path(), "1.0.0", "exit 0");
        assert_eq!(
            update_cli(cli, &["update"], || Some(installed))
                .await
                .expect("updates"),
            UpdateOutcome::Updated(Some(semver::Version::new(3, 1, 0)))
        );
    }

    #[tokio::test]
    async fn failed_updates_explain_why() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let cli = fake_cli(
            dir.path(),
            "1.0.0",
            "echo 'Checking for updates' >&2\necho 'Permission denied' >&2\nexit 1",
        );
        let error = update_cli(cli.clone(), &["update"], || Some(cli))
            .await
            .expect_err("fails");
        assert_eq!(error.0, "Permission denied");
        let silent = fake_cli(dir.path(), "1.0.0", "exit 3");
        let error = update_cli(silent.clone(), &["update"], || Some(silent))
            .await
            .expect_err("fails");
        assert_eq!(error.0, "Exit code 3");
    }

    #[tokio::test]
    async fn run_cli_passes_each_argument_whole_and_reports_stderr() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let record = dir.path().join("arguments");
        let cli = fake_cli(
            dir.path(),
            "1.0.0",
            &format!(
                "for argument in \"$@\"; do echo \"$argument\" >> '{}'; done
echo 'no such server' >&2
exit 1",
                record.display()
            ),
        );
        let arguments = ["mcp", "remove", "my server; rm -rf x"].map(String::from);
        let error = run_cli(&cli, &arguments, dir.path(), None)
            .await
            .expect_err("fails");
        assert_eq!(error.0, "no such server");
        let recorded = std::fs::read_to_string(record).expect("recorded arguments");
        assert_eq!(
            recorded.lines().collect::<Vec<_>>(),
            ["mcp", "remove", "my server; rm -rf x"]
        );
    }

    #[tokio::test]
    async fn run_cli_writes_the_input_to_stdin() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let record = dir.path().join("stdin");
        let cli = fake_cli(
            dir.path(),
            "1.0.0",
            &format!("cat > '{}'", record.display()),
        );
        run_cli(&cli, &[], dir.path(), Some("{\"op\":\"add\"}"))
            .await
            .expect("runs");
        assert_eq!(
            std::fs::read_to_string(record).expect("recorded input"),
            "{\"op\":\"add\"}"
        );
    }

    #[tokio::test]
    async fn run_cli_reports_why_a_cli_that_exits_without_reading_its_input() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let cli = fake_cli(
            dir.path(),
            "1.0.0",
            "echo 'refusing to run' >&2
exit 2",
        );
        let input = "x".repeat(8 * 1024 * 1024);
        let error = run_cli(&cli, &[], dir.path(), Some(&input))
            .await
            .expect_err("fails");
        assert_eq!(error.0, "refusing to run");
    }
}
