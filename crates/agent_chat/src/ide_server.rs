use std::io::Write as _;
use std::net::Ipv4Addr;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::str::FromStr as _;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::Context as _;
use async_tungstenite::tungstenite::Message;
use async_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use async_tungstenite::tungstenite::protocol::CloseFrame;
use async_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use collections::HashMap;
use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{App, AppContext as _, Entity, EntityId, Global, Task, TaskExt as _};
use language::{Diagnostic, PointUtf16};
use lsp::{DiagnosticSeverity, NumberOrString};
use project::{Project, ProjectPath};
use serde_json::{Value, json};
use util::ResultExt as _;
use uuid::Uuid;
use workspace::Workspace;

/// Tools served over MCP, whatever carries the messages.
pub(crate) trait IdeTools {
    fn call(&self, name: &str, arguments: &Value) -> anyhow::Result<Vec<String>>;
    fn list(&self) -> Value;
}

const FALLBACK_PROTOCOL_VERSION: &str = "2025-06-18";

pub(crate) fn handle_mcp_message(message: &Value, tools: &dyn IdeTools) -> Value {
    // The CLI expects this exact reply to every notification.
    let Some(id) = message.get("id").cloned() else {
        return json!({ "jsonrpc": "2.0", "result": {}, "id": 0 });
    };
    let params = message.get("params");
    let result = match message.get("method").and_then(Value::as_str) {
        Some("initialize") => Ok(json!({
            "protocolVersion": params
                .and_then(|params| params.get("protocolVersion"))
                .cloned()
                .unwrap_or_else(|| json!(FALLBACK_PROTOCOL_VERSION)),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "wu", "version": env!("CARGO_PKG_VERSION") },
        })),
        Some("tools/list") => Ok(json!({ "tools": tools.list() })),
        Some("tools/call") => {
            let name = params
                .and_then(|params| params.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = params
                .and_then(|params| params.get("arguments"))
                .unwrap_or(&Value::Null);
            tools
                .call(name, arguments)
                .map(|texts| {
                    let content = texts
                        .into_iter()
                        .map(|text| json!({ "type": "text", "text": text }))
                        .collect::<Vec<_>>();
                    json!({ "content": content })
                })
                .map_err(|error| (-32603, format!("{error:#}")))
        }
        Some(method) => Err((-32601, format!("Method not found: {method}"))),
        None => Err((-32600, "Invalid request: no method".to_string())),
    };
    match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    }
}

const AUTH_HEADER: &str = "x-claude-code-ide-authorization";

type Client = Arc<Mutex<Option<mpsc::UnboundedSender<Message>>>>;

/// Lets a `claude` started in wu's terminal use wu as its IDE; the CLI finds it through the lock file.
pub struct IdeServer {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "terminals don't get CLAUDE_CODE_SSE_PORT or editor notifications yet"
        )
    )]
    pub port: u16,
    pub auth_token: String,
    lock_path: PathBuf,
    client: Client,
    _accept: Task<()>,
}

impl IdeServer {
    pub fn start(
        workspace_folders: Vec<PathBuf>,
        tools: Arc<dyn IdeTools + Send + Sync>,
        cx: &mut App,
    ) -> anyhow::Result<Self> {
        Self::start_in(&lock_directory(), workspace_folders, tools, cx)
    }

    fn start_in(
        directory: &Path,
        workspace_folders: Vec<PathBuf>,
        tools: Arc<dyn IdeTools + Send + Sync>,
        cx: &mut App,
    ) -> anyhow::Result<Self> {
        // The OS picks a free ephemeral port, which every platform keeps within the CLI's expected 10000-65535.
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .context("binding the IDE server")?;
        let port = listener.local_addr()?.port();
        let listener = smol::net::TcpListener::try_from(listener)?;
        create_lock_directory(directory)
            .with_context(|| format!("creating {}", directory.display()))?;
        let auth_token = Uuid::new_v4().to_string();
        let lock_path = directory.join(format!("{port}.lock"));
        write_lock_file(&lock_path, &workspace_folders, &auth_token)
            .with_context(|| format!("writing {}", lock_path.display()))?;
        let client = Client::default();
        let executor = cx.background_executor().clone();
        let accept = cx.background_spawn({
            let auth_token = auth_token.clone();
            let client = client.clone();
            async move {
                loop {
                    match listener.accept().await {
                        Ok((stream, _)) => executor
                            .spawn(serve_connection(
                                stream,
                                auth_token.clone(),
                                tools.clone(),
                                client.clone(),
                            ))
                            .detach(),
                        Err(error) => {
                            log::error!("IDE server stopped accepting connections: {error}");
                            return;
                        }
                    }
                }
            }
        });
        Ok(Self {
            port,
            auth_token,
            lock_path,
            client,
            _accept: accept,
        })
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "terminals don't get CLAUDE_CODE_SSE_PORT or editor notifications yet"
        )
    )]
    pub fn notify(&self, method: &str, params: Value) {
        if let Some(client) = lock_client(&self.client).as_ref() {
            let message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
            client
                .unbounded_send(Message::text(message.to_string()))
                .ok();
        }
    }

    fn set_workspace_folders(&self, folders: &[PathBuf]) -> std::io::Result<()> {
        write_lock_file(&self.lock_path, folders, &self.auth_token)
    }
}

impl Drop for IdeServer {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.lock_path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            log::error!("couldn't remove {}: {error}", self.lock_path.display());
        }
        if let Some(client) = lock_client(&self.client).take() {
            client.unbounded_send(Message::Close(None)).ok();
        }
    }
}

fn lock_client(client: &Client) -> MutexGuard<'_, Option<mpsc::UnboundedSender<Message>>> {
    client.lock().unwrap_or_else(PoisonError::into_inner)
}

async fn serve_connection(
    stream: smol::net::TcpStream,
    auth_token: String,
    tools: Arc<dyn IdeTools + Send + Sync>,
    client: Client,
) {
    if let Err(error) = serve_websocket(stream, &auth_token, tools.as_ref(), &client).await {
        log::warn!("IDE server connection ended: {error:#}");
    }
}

async fn serve_websocket(
    stream: smol::net::TcpStream,
    auth_token: &str,
    tools: &(dyn IdeTools + Send + Sync),
    client: &Client,
) -> anyhow::Result<()> {
    let mut authorized = false;
    // The error type is fixed by tungstenite's handshake callback.
    #[allow(clippy::result_large_err)]
    let check_token = |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
        authorized = request
            .headers()
            .get(AUTH_HEADER)
            .is_some_and(|token| tokens_match(token.as_bytes(), auth_token.as_bytes()));
        Ok(response)
    };
    let mut websocket = async_tungstenite::accept_hdr_async(stream, check_token).await?;
    if !authorized {
        let frame = CloseFrame {
            code: CloseCode::Policy,
            reason: "Unauthorized".into(),
        };
        websocket.close(Some(frame)).await?;
        return Ok(());
    }

    let (sender, mut outgoing) = mpsc::unbounded();
    if let Some(previous) = lock_client(client).replace(sender.clone()) {
        let frame = CloseFrame {
            code: CloseCode::Library(4001),
            reason: "Another Claude Code session connected to this IDE".into(),
        };
        previous.unbounded_send(Message::Close(Some(frame))).ok();
    }
    let (mut sink, mut incoming) = websocket.split();
    let write = async {
        while let Some(message) = outgoing.next().await {
            let closing = matches!(message, Message::Close(_));
            sink.send(message).await?;
            if closing {
                break;
            }
        }
        anyhow::Ok(())
    };
    let read = async {
        while let Some(message) = incoming.next().await {
            if let Message::Text(text) = message?
                && let Some(reply) = reply_to(&text, tools)
            {
                sender.unbounded_send(Message::text(reply.to_string()))?;
            }
        }
        anyhow::Ok(())
    };
    let result = futures::future::select(pin!(write), pin!(read))
        .await
        .factor_first()
        .0;
    let mut current = lock_client(client);
    if current
        .as_ref()
        .is_some_and(|current| current.same_receiver(&sender))
    {
        *current = None;
    }
    result
}

/// Unlike the in-process channel, a socket must not answer notifications, nor the client's replies to requests.
fn reply_to(text: &str, tools: &dyn IdeTools) -> Option<Value> {
    let message = serde_json::from_str::<Value>(text).log_err()?;
    let is_request = message.get("id").is_some() && message.get("method").is_some();
    is_request.then(|| handle_mcp_message(&message, tools))
}

/// Looks at every byte, so the time taken doesn't reveal how much of a guessed token was right.
fn tokens_match(given: &[u8], expected: &[u8]) -> bool {
    given.len() == expected.len()
        && given
            .iter()
            .zip(expected)
            .fold(0, |difference, (given, expected)| {
                difference | (given ^ expected)
            })
            == 0
}

fn lock_directory() -> PathBuf {
    agent_harness::claude::claude_config_dir().join("ide")
}

fn create_lock_directory(directory: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(directory)
}

fn write_lock_file(path: &Path, folders: &[PathBuf], auth_token: &str) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let lock = lock_file_json(std::process::id(), folders, auth_token);
    options.open(path)?.write_all(lock.to_string().as_bytes())
}

pub(crate) fn lock_file_json(pid: u32, folders: &[PathBuf], auth_token: &str) -> Value {
    let folders = folders
        .iter()
        .map(|folder| folder.to_string_lossy())
        .collect::<Vec<_>>();
    json!({
        "pid": pid,
        "workspaceFolders": folders,
        "ideName": "Wu",
        "transport": "ws",
        "runningInWindows": cfg!(windows),
        "authToken": auth_token,
    })
}

/// Removes the lock files a crashed wu left behind; other IDEs' lock files are theirs to clean up.
pub(crate) fn remove_stale_lock_files(
    directory: &Path,
    is_alive: impl Fn(u32) -> bool,
) -> std::io::Result<usize> {
    let mut removed = 0;
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension() != Some("lock".as_ref()) {
            continue;
        }
        let Some(lock) = std::fs::read(&path)
            .ok()
            .and_then(|contents| serde_json::from_slice::<Value>(&contents).ok())
        else {
            continue;
        };
        let pid = lock.get("pid").and_then(Value::as_u64);
        if lock.get("ideName").and_then(Value::as_str) == Some("Wu")
            && let Some(pid) = pid.and_then(|pid| u32::try_from(pid).ok())
            && !is_alive(pid)
        {
            std::fs::remove_file(&path)?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn process_is_alive(pid: u32) -> bool {
    let system = sysinfo::System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_processes(sysinfo::ProcessRefreshKind::nothing()),
    );
    system.process(sysinfo::Pid::from_u32(pid)).is_some()
}

struct GlobalIdeServer {
    server: IdeServer,
    folders: HashMap<EntityId, Vec<PathBuf>>,
}

impl Global for GlobalIdeServer {}

pub(crate) fn init(cx: &mut App) {
    let server = match IdeServer::start(Vec::new(), Arc::new(TerminalTools), cx) {
        Ok(server) => server,
        Err(error) => {
            log::error!("couldn't start the IDE server for Claude in the terminal: {error:#}");
            return;
        }
    };
    cx.set_global(GlobalIdeServer {
        server,
        folders: HashMap::default(),
    });
    let directory = lock_directory();
    cx.background_spawn(async move {
        remove_stale_lock_files(&directory, process_is_alive)
            .with_context(|| format!("removing stale lock files in {}", directory.display()))
    })
    .detach_and_log_err(cx);
    cx.observe_new(|workspace: &mut Workspace, _, cx| {
        let project = workspace.project().clone();
        if !project.read(cx).is_local() {
            return;
        }
        let workspace_id = cx.entity_id();
        set_workspace_folders(workspace_id, Some(&project), cx);
        cx.subscribe(&project, move |_, project, event, cx| {
            if let project::Event::WorktreeAdded(_) | project::Event::WorktreeRemoved(_) = event {
                set_workspace_folders(workspace_id, Some(&project), cx);
            }
        })
        .detach();
        cx.on_release(move |_, cx| set_workspace_folders(workspace_id, None, cx))
            .detach();
    })
    .detach();
}

/// `project` is `None` once the workspace is gone.
fn set_workspace_folders(workspace_id: EntityId, project: Option<&Entity<Project>>, cx: &mut App) {
    let folders = project.map(|project| {
        project
            .read(cx)
            .visible_worktrees(cx)
            .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
            .collect::<Vec<_>>()
    });
    if !cx.has_global::<GlobalIdeServer>() {
        return;
    }
    let global = cx.global_mut::<GlobalIdeServer>();
    match folders {
        Some(folders) => global.folders.insert(workspace_id, folders),
        None => global.folders.remove(&workspace_id),
    };
    let mut all_folders = global
        .folders
        .values()
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    all_folders.sort();
    all_folders.dedup();
    global.server.set_workspace_folders(&all_folders).log_err();
}

/// The tools a `claude` running in wu's terminal gets.
struct TerminalTools;

impl IdeTools for TerminalTools {
    fn call(&self, name: &str, _: &Value) -> anyhow::Result<Vec<String>> {
        anyhow::bail!("Unknown tool: {name}")
    }

    fn list(&self) -> Value {
        json!([])
    }
}

/// The tools a panel chat gets from its own project.
pub(crate) struct PanelTools<'a> {
    pub project: Option<Entity<Project>>,
    pub cx: &'a App,
}

impl IdeTools for PanelTools<'_> {
    fn call(&self, name: &str, arguments: &Value) -> anyhow::Result<Vec<String>> {
        anyhow::ensure!(name == "getDiagnostics", "Unknown tool: {name}");
        let project = self
            .project
            .as_ref()
            .context("This chat has no project open.")?;
        let uri = arguments.get("uri").and_then(Value::as_str);
        let minimum_severity = arguments.get("minSeverity").and_then(Value::as_str);
        Ok(vec![
            diagnostics_json(project, uri, minimum_severity, self.cx).to_string(),
        ])
    }

    fn list(&self) -> Value {
        json!([{
            "name": "getDiagnostics",
            "description": "Get the errors, warnings and other diagnostics the editor currently shows, including those for unsaved changes in open files. Returns a JSON array with one entry per file, each listing that file's diagnostics. Pass `uri` to check one file; its entry is returned even when it has no diagnostics. Omit `uri` to get only the files that have at least one diagnostic. Pass `minSeverity` to include only diagnostics at least that severe. Language servers may only analyze some files, so a missing file or an empty list can mean the file has not been checked, not that it is clean.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "uri": {
                        "type": "string",
                        "description": "file:// URI of the one file to check. Omit to get every file that has at least one diagnostic."
                    },
                    "minSeverity": {
                        "type": "string",
                        "enum": SEVERITIES,
                        "description": "Include only diagnostics at least this severe. From most to least severe: Error, Warning, Information, Hint. Omit to include every severity."
                    }
                }
            },
            "annotations": { "title": "Get diagnostics", "readOnlyHint": true }
        }])
    }
}

const SEVERITIES: [&str; 4] = ["Error", "Warning", "Information", "Hint"];

pub(crate) fn diagnostics_json(
    project: &Entity<Project>,
    uri: Option<&str>,
    minimum_severity: Option<&str>,
    cx: &App,
) -> Value {
    let project = project.read(cx);
    let minimum_rank = minimum_severity
        .and_then(|severity| SEVERITIES.iter().position(|name| *name == severity))
        .unwrap_or(SEVERITIES.len() - 1);
    let file_json = |uri: String, project_path: Option<&ProjectPath>| {
        let (lines_in_file, diagnostics) = project_path
            .map(|project_path| file_diagnostics(project, project_path, minimum_rank, cx))
            .unwrap_or_default();
        let mut file = json!({ "uri": uri, "diagnostics": diagnostics });
        if let Some(lines_in_file) = lines_in_file {
            file["linesInFile"] = json!(lines_in_file);
        }
        file
    };
    if let Some(uri) = uri {
        let path = lsp::Uri::from_str(uri)
            .ok()
            .and_then(|uri| uri.to_file_path().ok())
            .unwrap_or_else(|| PathBuf::from(uri));
        let project_path = project.find_project_path(&path, cx);
        return json!([file_json(uri.to_string(), project_path.as_ref())]);
    }
    let mut paths = project
        .diagnostic_summaries(false, cx)
        .map(|(project_path, _, _)| project_path)
        .collect::<Vec<_>>();
    paths.dedup();
    let files = paths
        .iter()
        .filter_map(|project_path| {
            let path = project.absolute_path(project_path, cx)?;
            let uri = lsp::Uri::from_file_path(path).ok()?;
            let file = file_json(uri.to_string(), Some(project_path));
            let has_diagnostics = file["diagnostics"]
                .as_array()
                .is_some_and(|diagnostics| !diagnostics.is_empty());
            has_diagnostics.then_some(file)
        })
        .collect::<Vec<_>>();
    Value::Array(files)
}

/// An open buffer has the diagnostics moved along with unsaved edits, and a line count to report.
fn file_diagnostics(
    project: &Project,
    project_path: &ProjectPath,
    minimum_rank: usize,
    cx: &App,
) -> (Option<u32>, Vec<Value>) {
    if let Some(buffer) = project.get_open_buffer(project_path, cx) {
        let snapshot = buffer.read(cx).snapshot();
        let diagnostics = snapshot
            .diagnostics_in_range::<_, PointUtf16>(0..snapshot.len(), false)
            .filter_map(|entry| diagnostic_json(entry.diagnostic, entry.range, minimum_rank))
            .collect();
        return (Some(snapshot.max_point().row + 1), diagnostics);
    }
    let diagnostics = project
        .lsp_store()
        .read(cx)
        .stored_diagnostics(project_path)
        .filter_map(|entry| {
            let range = entry.range.start.0..entry.range.end.0;
            diagnostic_json(&entry.diagnostic, range, minimum_rank)
        })
        .collect();
    (None, diagnostics)
}

fn diagnostic_json(
    diagnostic: &Diagnostic,
    range: Range<PointUtf16>,
    minimum_rank: usize,
) -> Option<Value> {
    // Related information is stored as extra non-primary entries; the Problems list shows only the diagnostic itself.
    if !diagnostic.is_primary {
        return None;
    }
    let (rank, severity) = match diagnostic.severity {
        DiagnosticSeverity::ERROR => (0, "Error"),
        DiagnosticSeverity::WARNING => (1, "Warning"),
        DiagnosticSeverity::INFORMATION => (2, "Information"),
        _ => (3, "Hint"),
    };
    let code = diagnostic.code.as_ref().map(|code| match code {
        NumberOrString::Number(number) => number.to_string(),
        NumberOrString::String(text) => text.clone(),
    });
    (rank <= minimum_rank).then(|| {
        json!({
            "message": diagnostic.message.as_ref(),
            "severity": severity,
            "range": {
                "start": { "line": range.start.row, "character": range.start.column },
                "end": { "line": range.end.row, "character": range.end.column },
            },
            "source": diagnostic.source,
            "code": code,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_tungstenite::tungstenite::client::IntoClientRequest as _;
    use gpui::TestAppContext;
    use language::{DiagnosticEntry, DiagnosticMessage, DiagnosticSourceKind, Unclipped};
    use lsp::LanguageServerId;
    use project::FakeFs;
    use util::path;

    struct FakeTools;

    impl IdeTools for FakeTools {
        fn call(&self, name: &str, _: &Value) -> anyhow::Result<Vec<String>> {
            anyhow::ensure!(name == "getDiagnostics", "Unknown tool: {name}");
            Ok(vec!["[]".to_string()])
        }

        fn list(&self) -> Value {
            json!([{ "name": "getDiagnostics" }])
        }
    }

    fn request(id: u64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    #[test]
    fn handle_mcp_message_initialize_lists_and_calls() {
        let initialize = handle_mcp_message(
            &request(0, "initialize", json!({ "protocolVersion": "2025-11-25" })),
            &FakeTools,
        );
        assert_eq!(initialize["id"], 0);
        assert_eq!(initialize["result"]["protocolVersion"], "2025-11-25");
        assert!(initialize["result"]["capabilities"]["tools"].is_object());

        let list = handle_mcp_message(&request(1, "tools/list", json!({})), &FakeTools);
        assert_eq!(list["result"]["tools"][0]["name"], "getDiagnostics");
        assert_eq!(list["result"]["tools"].as_array().map(Vec::len), Some(1));

        let call = handle_mcp_message(
            &request(
                2,
                "tools/call",
                json!({ "name": "getDiagnostics", "arguments": {} }),
            ),
            &FakeTools,
        );
        assert_eq!(call["id"], 2);
        assert_eq!(call["result"]["content"][0]["type"], "text");
        assert_eq!(call["result"]["content"][0]["text"], "[]");

        let notification = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert_eq!(
            handle_mcp_message(&notification, &FakeTools),
            json!({ "jsonrpc": "2.0", "result": {}, "id": 0 })
        );

        let unknown = handle_mcp_message(
            &request(3, "tools/call", json!({ "name": "openDiff" })),
            &FakeTools,
        );
        assert_eq!(unknown["id"], 3);
        assert!(unknown["error"]["message"].is_string(), "{unknown}");

        for malformed in [
            json!(null),
            json!("text"),
            json!({ "id": 4 }),
            json!({ "id": 5, "method": 7 }),
        ] {
            assert!(handle_mcp_message(&malformed, &FakeTools).is_object());
        }
    }

    #[test]
    fn lock_file_has_the_expected_fields() {
        let lock = lock_file_json(42, &[PathBuf::from(path!("/root"))], "token");
        assert_eq!(lock["pid"], 42);
        assert_eq!(lock["workspaceFolders"], json!([path!("/root")]));
        assert_eq!(lock["ideName"], "Wu");
        assert_eq!(lock["transport"], "ws");
        assert_eq!(lock["runningInWindows"], cfg!(windows));
        assert_eq!(lock["authToken"], "token");
    }

    #[test]
    fn removes_stale_lock_files() {
        let directory = tempfile::tempdir().expect("temp dir");
        let write = |name: &str, contents: String| {
            std::fs::write(directory.path().join(name), contents).expect("write lock file")
        };
        write("10001.lock", lock_file_json(1, &[], "dead").to_string());
        write("10002.lock", lock_file_json(2, &[], "live").to_string());
        write("10003.lock", "not json".to_string());
        let mut other_ide = lock_file_json(3, &[], "other");
        other_ide["ideName"] = json!("Visual Studio Code");
        write("10004.lock", other_ide.to_string());

        let removed = remove_stale_lock_files(directory.path(), |pid| pid == 2);

        assert_eq!(removed.expect("directory is readable"), 1);
        let mut left = std::fs::read_dir(directory.path())
            .expect("directory is readable")
            .map(|entry| entry.expect("entry").file_name())
            .collect::<Vec<_>>();
        left.sort();
        assert_eq!(left, ["10002.lock", "10003.lock", "10004.lock"]);
    }

    type Client = async_tungstenite::WebSocketStream<smol::net::TcpStream>;

    fn start_server(cx: &mut TestAppContext) -> (IdeServer, tempfile::TempDir) {
        cx.executor().allow_parking();
        let directory = tempfile::tempdir().expect("temp dir");
        let server = cx
            .update(|cx| {
                IdeServer::start_in(
                    directory.path(),
                    vec![PathBuf::from(path!("/root"))],
                    Arc::new(FakeTools),
                    cx,
                )
            })
            .expect("server starts");
        (server, directory)
    }

    async fn connect(port: u16, auth_token: Option<&str>) -> Client {
        let mut request = format!("ws://127.0.0.1:{port}")
            .into_client_request()
            .expect("valid request");
        if let Some(auth_token) = auth_token {
            request
                .headers_mut()
                .insert(AUTH_HEADER, auth_token.parse().expect("valid header value"));
        }
        let stream = smol::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("server listens");
        async_tungstenite::client_async(request, stream)
            .await
            .expect("handshake succeeds")
            .0
    }

    async fn next_json(client: &mut Client) -> Value {
        match client.next().await {
            Some(Ok(Message::Text(text))) => serde_json::from_str(&text).expect("JSON message"),
            other => panic!("expected a text message, got {other:?}"),
        }
    }

    async fn close_code(client: &mut Client) -> u16 {
        match client.next().await {
            Some(Ok(Message::Close(Some(frame)))) => frame.code.into(),
            other => panic!("expected a close frame, got {other:?}"),
        }
    }

    async fn list_tools(client: &mut Client, id: u64) -> Value {
        client
            .send(Message::text(
                request(id, "tools/list", json!({})).to_string(),
            ))
            .await
            .expect("send");
        next_json(client).await
    }

    #[gpui::test]
    async fn rejects_wrong_token(cx: &mut TestAppContext) {
        let (server, _directory) = start_server(cx);

        let mut without_token = connect(server.port, None).await;
        assert_eq!(close_code(&mut without_token).await, 1008);

        let mut wrong_token = connect(server.port, Some("not-the-token")).await;
        assert_eq!(close_code(&mut wrong_token).await, 1008);
    }

    #[gpui::test]
    async fn tools_call_over_websocket(cx: &mut TestAppContext) {
        let (server, directory) = start_server(cx);
        let lock_path = directory.path().join(format!("{}.lock", server.port));
        let lock: Value =
            serde_json::from_slice(&std::fs::read(&lock_path).expect("lock file written"))
                .expect("lock file is JSON");
        assert_eq!(lock["authToken"], server.auth_token);
        assert_eq!(lock["pid"], std::process::id());

        let mut client = connect(server.port, Some(&server.auth_token)).await;
        // A notification gets no reply, so the next message must answer the request after it.
        client
            .send(Message::text(
                json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string(),
            ))
            .await
            .expect("send");
        let list = list_tools(&mut client, 1).await;
        assert_eq!(list["id"], 1);
        assert_eq!(list["result"]["tools"][0]["name"], "getDiagnostics");

        drop(server);
        assert!(!lock_path.exists(), "lock file is removed on drop");
    }

    #[gpui::test]
    async fn second_connection_replaces_the_first(cx: &mut TestAppContext) {
        let (server, _directory) = start_server(cx);
        let mut first = connect(server.port, Some(&server.auth_token)).await;
        assert_eq!(list_tools(&mut first, 1).await["id"], 1);

        let mut second = connect(server.port, Some(&server.auth_token)).await;
        assert_eq!(list_tools(&mut second, 2).await["id"], 2);
        assert_eq!(close_code(&mut first).await, 4001);

        server.notify("selection_changed", json!({ "text": "" }));
        let notification = next_json(&mut second).await;
        assert_eq!(notification["method"], "selection_changed");
        assert!(notification.get("id").is_none(), "{notification}");
    }

    #[gpui::test]
    async fn diagnostics_json_shape(cx: &mut TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({ "main.rs": "fn main() {\n    let x: u8 = 300;\n}\n", "clean.rs": "" }),
        )
        .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        project
            .update(cx, |project, cx| {
                project.lsp_store().update(cx, |lsp_store, cx| {
                    lsp_store.update_diagnostic_entries(
                        LanguageServerId(0),
                        PathBuf::from(path!("/root/main.rs")),
                        None,
                        None,
                        vec![DiagnosticEntry::new(
                            Unclipped(PointUtf16::new(1, 16))..Unclipped(PointUtf16::new(1, 19)),
                            Diagnostic {
                                severity: DiagnosticSeverity::ERROR,
                                message: DiagnosticMessage::from("literal out of range for `u8`"),
                                source: Some("rustc".to_string()),
                                source_kind: DiagnosticSourceKind::Pushed,
                                is_primary: true,
                                ..Default::default()
                            },
                        )],
                        cx,
                    )
                })
            })
            .expect("diagnostics are stored");
        cx.run_until_parked();

        let unopened = cx.update(|cx| diagnostics_json(&project, None, None, cx));
        assert_eq!(unopened.as_array().map(Vec::len), Some(1), "{unopened}");
        assert!(
            unopened[0]["uri"]
                .as_str()
                .is_some_and(|uri| uri.starts_with("file://"))
        );
        assert!(unopened[0].get("linesInFile").is_none(), "{unopened}");
        assert_eq!(unopened[0]["diagnostics"][0]["severity"], "Error");
        assert_eq!(
            unopened[0]["diagnostics"][0]["range"],
            json!({ "start": { "line": 1, "character": 16 }, "end": { "line": 1, "character": 19 } })
        );

        let _open_buffer = project
            .update(cx, |project, cx| {
                project.open_local_buffer(path!("/root/main.rs"), cx)
            })
            .await
            .expect("buffer opens");
        let opened = cx.update(|cx| diagnostics_json(&project, None, None, cx));
        assert_eq!(opened[0]["linesInFile"], 4);
        assert_eq!(opened[0]["diagnostics"][0]["severity"], "Error");
        assert_eq!(
            opened[0]["diagnostics"][0]["message"],
            "literal out of range for `u8`"
        );
        assert_eq!(opened[0]["diagnostics"][0]["source"], "rustc");
        assert_eq!(
            opened[0]["diagnostics"][0]["range"]["start"],
            json!({ "line": 1, "character": 16 })
        );

        let warnings_only = cx.update(|cx| diagnostics_json(&project, None, Some("Warning"), cx));
        assert_eq!(warnings_only[0]["diagnostics"][0]["severity"], "Error");

        let clean_uri = lsp::Uri::from_file_path(path!("/root/clean.rs"))
            .expect("absolute path")
            .to_string();
        let clean = cx.update(|cx| diagnostics_json(&project, Some(&clean_uri), None, cx));
        assert_eq!(clean, json!([{ "uri": clean_uri, "diagnostics": [] }]));
    }
}
