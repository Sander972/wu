use std::io::Write as _;
use std::net::Ipv4Addr;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::str::FromStr as _;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use async_tungstenite::tungstenite::Message;
use async_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use async_tungstenite::tungstenite::protocol::CloseFrame;
use async_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use collections::HashMap;
use editor::{Editor, EditorEvent};
use futures::channel::{mpsc, oneshot};
use futures::future::{BoxFuture, Either};
use futures::stream::FuturesUnordered;
use futures::{FutureExt as _, StreamExt as _};
use gpui::{
    App, AppContext as _, BackgroundExecutor, Entity, EntityId, Focusable as _, Global, Task,
    TaskExt as _, WeakEntity, Window, WindowHandle,
};
use language::{Diagnostic, PointUtf16};
use lsp::{DiagnosticSeverity, NumberOrString};
use project::{Project, ProjectPath};
use serde_json::{Value, json};
use settings::Settings as _;
use terminal::Terminal;
use util::ResultExt as _;
use uuid::Uuid;
use workspace::{MultiWorkspace, Workspace, item::ItemEvent};

use crate::AgentChatSettings;
use crate::chat_view::{self, ProposedEditReview};

/// Tools served over MCP, whatever carries the messages.
pub(crate) trait IdeTools {
    /// Called where the request was read. A tool that needs the editor or waits for the user does so in the future it returns, so requests that arrive meanwhile are still answered.
    fn call(
        &self,
        name: &str,
        arguments: &Value,
    ) -> BoxFuture<'static, anyhow::Result<Vec<String>>>;
    fn list(&self) -> Value;
}

const FALLBACK_PROTOCOL_VERSION: &str = "2025-06-18";

pub(crate) fn handle_mcp_message(
    message: &Value,
    tools: &dyn IdeTools,
) -> BoxFuture<'static, Value> {
    // The CLI expects this exact reply to every notification.
    let Some(id) = message.get("id").cloned() else {
        return futures::future::ready(json!({ "jsonrpc": "2.0", "result": {}, "id": 0 })).boxed();
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
            let call = tools.call(name, arguments);
            return async move {
                let result = call
                    .await
                    .map(|texts| {
                        let content = texts
                            .into_iter()
                            .map(|text| json!({ "type": "text", "text": text }))
                            .collect::<Vec<_>>();
                        json!({ "content": content })
                    })
                    .map_err(|error| (-32603, format!("{error:#}")));
                rpc_reply(id, result)
            }
            .boxed();
        }
        Some(method) => Err((-32601, format!("Method not found: {method}"))),
        None => Err((-32600, "Invalid request: no method".to_string())),
    };
    futures::future::ready(rpc_reply(id, result)).boxed()
}

fn rpc_reply(id: Value, result: Result<Value, (i64, String)>) -> Value {
    match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    }
}

fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

const AUTH_HEADER: &str = "x-claude-code-ide-authorization";
/// The extension waits as long after a client connects before sending it the current selection.
const SELECTION_RESEND_DELAY: Duration = Duration::from_millis(500);
const SELECTION_DEBOUNCE: Duration = Duration::from_millis(300);

type Client = Arc<Mutex<Option<mpsc::UnboundedSender<Message>>>>;
type LatestSelection = Arc<Mutex<Option<Value>>>;

/// Lets a `claude` started in wu's terminal use wu as its IDE; the CLI finds it through the lock file.
pub struct IdeServer {
    pub port: u16,
    pub auth_token: String,
    lock_path: PathBuf,
    client: Client,
    latest_selection: LatestSelection,
    _accept: Task<()>,
}

impl IdeServer {
    fn start_in(
        directory: &Path,
        workspace_folders: Vec<PathBuf>,
        tools: Arc<dyn IdeTools + Send + Sync>,
        cx: &mut App,
    ) -> anyhow::Result<Self> {
        // The OS picks a free port, so nothing can take it between choosing and binding. OS ephemeral ranges are configurable, so it may fall outside the 10000-65535 the extension draws from; the CLI connects to whichever port the lock file names.
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
        let latest_selection = LatestSelection::default();
        let executor = cx.background_executor().clone();
        let accept = cx.background_spawn({
            let auth_token = auth_token.clone();
            let client = client.clone();
            let latest_selection = latest_selection.clone();
            async move {
                loop {
                    match listener.accept().await {
                        Ok((stream, _)) => executor
                            .spawn(serve_connection(
                                stream,
                                auth_token.clone(),
                                tools.clone(),
                                client.clone(),
                                latest_selection.clone(),
                                executor.clone(),
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
            latest_selection,
            _accept: accept,
        })
    }

    pub fn notify(&self, method: &str, params: Value) {
        if let Some(client) = lock(&self.client).as_ref() {
            client
                .unbounded_send(Message::text(notification(method, params).to_string()))
                .ok();
        }
    }

    fn is_connected(&self) -> bool {
        lock(&self.client).is_some()
    }

    /// Keeps the selection a newly connected client gets, and tells whether it differs from the one kept before.
    fn set_selection(&self, selection: Value) -> bool {
        let mut latest = lock(&self.latest_selection);
        if latest.as_ref() == Some(&selection) {
            return false;
        }
        *latest = Some(selection);
        true
    }

    fn notify_selection(&self) {
        let selection = lock(&self.latest_selection).clone();
        if let Some(selection) = selection {
            self.notify("selection_changed", selection);
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
        if let Some(client) = lock(&self.client).take() {
            client.unbounded_send(Message::Close(None)).ok();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

async fn serve_connection(
    stream: smol::net::TcpStream,
    auth_token: String,
    tools: Arc<dyn IdeTools + Send + Sync>,
    client: Client,
    latest_selection: LatestSelection,
    executor: BackgroundExecutor,
) {
    if let Err(error) = serve_websocket(
        stream,
        &auth_token,
        tools.as_ref(),
        &client,
        latest_selection,
        &executor,
    )
    .await
    {
        log::warn!("IDE server connection ended: {error:#}");
    }
}

async fn serve_websocket(
    stream: smol::net::TcpStream,
    auth_token: &str,
    tools: &(dyn IdeTools + Send + Sync),
    client: &Client,
    latest_selection: LatestSelection,
    executor: &BackgroundExecutor,
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
    if let Some(previous) = lock(client).replace(sender.clone()) {
        let frame = CloseFrame {
            code: CloseCode::Library(4001),
            reason: "Another Claude Code session connected to this IDE".into(),
        };
        previous.unbounded_send(Message::Close(Some(frame))).ok();
    }
    let (mut sink, incoming) = websocket.split();
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
        let resend_selection = executor.timer(SELECTION_RESEND_DELAY);
        let resend_selection = async move {
            resend_selection.await;
            let selection = lock(&latest_selection).clone();
            selection.map(|selection| notification("selection_changed", selection))
        };
        // Replies go out as their tools finish, so a call waiting for the user (`openDiff`) doesn't hold up the ones after it. Dropping them when the connection ends tells their tools nobody waits anymore.
        let mut pending = FuturesUnordered::<BoxFuture<'static, Option<Value>>>::new();
        pending.push(resend_selection.boxed());
        let mut incoming = incoming.fuse();
        loop {
            futures::select_biased! {
                reply = pending.select_next_some() => {
                    if let Some(reply) = reply {
                        sender.unbounded_send(Message::text(reply.to_string()))?;
                    }
                }
                message = incoming.next() => {
                    let Some(message) = message else {
                        break;
                    };
                    if let Message::Text(text) = message?
                        && let Some(reply) = reply_to(&text, tools)
                    {
                        pending.push(reply.map(Some).boxed());
                    }
                }
            }
        }
        anyhow::Ok(())
    };
    let result = futures::future::select(pin!(write), pin!(read))
        .await
        .factor_first()
        .0;
    let mut current = lock(client);
    if current
        .as_ref()
        .is_some_and(|current| current.same_receiver(&sender))
    {
        *current = None;
    }
    result
}

/// Unlike the in-process channel, a socket must not answer notifications, nor the client's replies to requests.
fn reply_to(text: &str, tools: &dyn IdeTools) -> Option<BoxFuture<'static, Value>> {
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

/// Written under another name and renamed into place, so the CLI never reads a half-written lock file.
fn write_lock_file(path: &Path, folders: &[PathBuf], auth_token: &str) -> std::io::Result<()> {
    let temporary_path = path.with_extension("lock.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&temporary_path)?;
    // The mode above applies only to a file it creates, and a crash can leave the temporary file behind.
    #[cfg(unix)]
    file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    let lock = lock_file_json(std::process::id(), folders, auth_token);
    file.write_all(lock.to_string().as_bytes())?;
    drop(file);
    std::fs::rename(&temporary_path, path)
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
    selection_debounce: Task<()>,
    _tool_calls: Task<()>,
}

impl Global for GlobalIdeServer {}

/// Kept while the server is stopped, so it starts with the folders already open.
#[derive(Default)]
struct WorkspaceFolders(HashMap<EntityId, Vec<PathBuf>>);

impl Global for WorkspaceFolders {}

pub(crate) fn init(cx: &mut App) {
    terminal::set_extra_env(terminal_env, cx);
    // Globals aren't dropped when wu exits, so the lock file is removed here.
    cx.on_app_quit(|cx| {
        set_running(false, cx);
        async {}
    })
    .detach();
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
    cx.observe_new(|editor: &mut Editor, window, cx| {
        let Some(window) = window else {
            return;
        };
        if !editor.mode().is_full() {
            return;
        }
        cx.subscribe_in(
            &cx.entity(),
            window,
            |editor, _, event: &EditorEvent, window, cx| {
                if matches!(event, EditorEvent::SelectionsChanged { local: true })
                    && cx.has_global::<GlobalIdeServer>()
                    && editor.focus_handle(cx).is_focused(window)
                    && let Some(selection) = chat_view::selection_json(editor, cx)
                {
                    selection_changed(selection, cx);
                }
            },
        )
        .detach();
    })
    .detach();
    set_running(AgentChatSettings::get_global(cx).enabled, cx);
}

/// Runs the server while Agent Chat is on; stopping it removes its lock file.
pub(crate) fn set_running(running: bool, cx: &mut App) {
    if !running {
        if cx.has_global::<GlobalIdeServer>() {
            cx.remove_global::<GlobalIdeServer>();
        }
        return;
    }
    if cx.has_global::<GlobalIdeServer>() {
        return;
    }
    let Some(directory) = lock_directory_for(cx) else {
        return;
    };
    let (tools, tool_calls) = terminal_tools(cx);
    let server = match IdeServer::start_in(&directory, all_workspace_folders(cx), tools, cx) {
        Ok(server) => server,
        Err(error) => {
            log::error!("couldn't start the IDE server for Claude in the terminal: {error:#}");
            return;
        }
    };
    cx.set_global(GlobalIdeServer {
        server,
        selection_debounce: Task::ready(()),
        _tool_calls: tool_calls,
    });
    cx.background_spawn(async move {
        remove_stale_lock_files(&directory, process_is_alive)
            .with_context(|| format!("removing stale lock files in {}", directory.display()))
    })
    .detach_and_log_err(cx);
}

fn lock_directory_for(cx: &App) -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(directory) = cx.try_global::<tests::TestLockDirectory>() {
        return Some(directory.0.clone());
    }
    // Tests run on a fake file system and must not leave lock files in the real Claude config directory.
    (!<dyn fs::Fs>::global(cx).is_fake()).then(lock_directory)
}

fn terminal_env(cx: &App) -> Vec<(String, String)> {
    cx.try_global::<GlobalIdeServer>()
        .map(|global| {
            vec![(
                "CLAUDE_CODE_SSE_PORT".to_string(),
                global.server.port.to_string(),
            )]
        })
        .unwrap_or_default()
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
    let known = &mut cx.default_global::<WorkspaceFolders>().0;
    match folders {
        Some(folders) => known.insert(workspace_id, folders),
        None => known.remove(&workspace_id),
    };
    if let Some(global) = cx.try_global::<GlobalIdeServer>() {
        global
            .server
            .set_workspace_folders(&all_workspace_folders(cx))
            .log_err();
    }
}

fn all_workspace_folders(cx: &App) -> Vec<PathBuf> {
    let mut folders = cx
        .try_global::<WorkspaceFolders>()
        .map(|known| known.0.values().flatten().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    folders.sort();
    folders.dedup();
    folders
}

fn selection_changed(selection: Value, cx: &mut App) {
    let Some(global) = cx.try_global::<GlobalIdeServer>() else {
        return;
    };
    if !global.server.set_selection(selection) {
        return;
    }
    let debounce = cx.spawn(async move |cx| {
        cx.background_executor().timer(SELECTION_DEBOUNCE).await;
        cx.update(|cx| {
            if let Some(global) = cx.try_global::<GlobalIdeServer>() {
                global.server.notify_selection();
            }
        });
    });
    cx.global_mut::<GlobalIdeServer>().selection_debounce = debounce;
}

/// Alt+K pressed with a terminal focused goes to the `claude` connected to wu, as `at_mentioned` with 0-based lines. Returns whether it went there.
pub(crate) fn mention_in_terminal(workspace: &Workspace, window: &Window, cx: &App) -> bool {
    let terminal_focused = workspace
        .focused_pane(window, cx)
        .read(cx)
        .active_item()
        .and_then(|item| item.act_as::<Terminal>(cx))
        .is_some();
    let Some(global) = cx
        .try_global::<GlobalIdeServer>()
        .filter(|global| terminal_focused && global.server.is_connected())
    else {
        return false;
    };
    let Some((path, lines)) = chat_view::terminal_mention_target(workspace, cx) else {
        return false;
    };
    global
        .server
        .notify("at_mentioned", at_mention_json(&path, lines));
    true
}

/// `lines` are 1-based and inclusive, as Alt+K resolves them.
fn at_mention_json(path: &Path, lines: Option<(u32, u32)>) -> Value {
    let mut mention = json!({ "filePath": path.to_string_lossy() });
    if let Some((start, end)) = lines {
        mention["lineStart"] = json!(start.saturating_sub(1));
        mention["lineEnd"] = json!(end.saturating_sub(1));
    }
    mention
}

type ToolReply = oneshot::Sender<anyhow::Result<Vec<String>>>;

struct ToolCall {
    name: String,
    arguments: Value,
    reply: ToolReply,
}

/// The tools a `claude` running in wu's terminal gets. Calls arrive on the server's threads and are answered on the main thread, which owns the editor; neither ever blocks waiting for the other.
struct TerminalTools {
    calls: mpsc::UnboundedSender<ToolCall>,
}

fn terminal_tools(cx: &mut App) -> (Arc<TerminalTools>, Task<()>) {
    let (calls, mut incoming) = mpsc::unbounded();
    let answering = cx.spawn(async move |cx| {
        while let Some(call) = incoming.next().await {
            cx.update(|cx| answer_tool_call(call, cx));
        }
    });
    (Arc::new(TerminalTools { calls }), answering)
}

impl IdeTools for TerminalTools {
    fn call(
        &self,
        name: &str,
        arguments: &Value,
    ) -> BoxFuture<'static, anyhow::Result<Vec<String>>> {
        let (reply, answer) = oneshot::channel();
        let sent = self
            .calls
            .unbounded_send(ToolCall {
                name: name.to_string(),
                arguments: arguments.clone(),
                reply,
            })
            .is_ok();
        // Stopping the server drops the calls it hadn't answered, which ends them here instead of leaving `claude` waiting.
        async move {
            anyhow::ensure!(sent, "Wu stopped serving tools to Claude.");
            answer
                .await
                .context("Wu stopped serving tools to Claude.")?
        }
        .boxed()
    }

    fn list(&self) -> Value {
        json!([
            {
                "name": "openDiff",
                "description": "Show a proposed change to a file in a diff tab in Wu and wait for the user to accept or reject it. Returns FILE_SAVED and the accepted contents, which the user may have edited, or DIFF_REJECTED and the tab name.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "old_file_path": { "type": "string", "description": "Path of the file as it is on disk." },
                        "new_file_path": { "type": "string", "description": "Path of the file once changed." },
                        "new_file_contents": { "type": "string", "description": "The proposed contents of the file." },
                        "tab_name": { "type": "string", "description": "Name of the diff tab." }
                    },
                    "required": ["old_file_path", "new_file_path", "new_file_contents", "tab_name"]
                }
            },
            diagnostics_tool(),
            {
                "name": "close_tab",
                "description": "Close the diff tab with this name.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "tab_name": { "type": "string" } },
                    "required": ["tab_name"]
                }
            },
            {
                "name": "closeAllDiffTabs",
                "description": "Close every diff tab openDiff opened.",
                "inputSchema": { "type": "object", "properties": {} }
            }
        ])
    }
}

fn answer_tool_call(call: ToolCall, cx: &mut App) {
    let ToolCall {
        name,
        arguments,
        reply,
    } = call;
    let result = match name.as_str() {
        "openDiff" => return open_diff(&arguments, reply, cx),
        "getDiagnostics" => {
            let uri = arguments.get("uri").and_then(Value::as_str);
            let minimum_severity = arguments.get("minSeverity").and_then(Value::as_str);
            Ok(vec![all_diagnostics(uri, minimum_severity, cx).to_string()])
        }
        "close_tab" => match arguments.get("tab_name").and_then(Value::as_str) {
            Some(tab_name) => {
                chat_view::close_diff_reviews(|name| name == tab_name, cx);
                Ok(vec!["TAB_CLOSED".to_string()])
            }
            None => Err(anyhow!("close_tab needs a string `tab_name`")),
        },
        "closeAllDiffTabs" => {
            let closed = chat_view::close_diff_reviews(|_| true, cx);
            Ok(vec![format!("CLOSED_{closed}_DIFF_TABS")])
        }
        _ => Err(anyhow!("Unknown tool: {name}")),
    };
    reply.send(result).ok();
}

fn open_diff(arguments: &Value, reply: ToolReply, cx: &mut App) {
    let opening = match start_diff(arguments, cx) {
        Ok(opening) => opening,
        Err(error) => {
            reply.send(Err(error)).ok();
            return;
        }
    };
    cx.spawn(async move |cx| {
        let (review, answered) = match opening.await {
            Ok(opened) => opened,
            Err(error) => {
                reply.send(Err(error)).ok();
                return;
            }
        };
        let mut reply = reply;
        // `claude` stops waiting when it disconnects or gets its answer in the terminal.
        let answer = match futures::future::select(answered, pin!(reply.cancellation())).await {
            Either::Left((answer, _)) => answer.ok(),
            Either::Right(_) => None,
        };
        if let Some(answer) = answer {
            reply.send(Ok(answer.to_vec())).ok();
        }
        review
            .update(cx, |_, cx| cx.emit(ItemEvent::CloseItem))
            .ok();
    })
    .detach();
}

type OpeningDiff = Task<
    anyhow::Result<(
        WeakEntity<ProposedEditReview>,
        oneshot::Receiver<[String; 2]>,
    )>,
>;

fn start_diff(arguments: &Value, cx: &mut App) -> anyhow::Result<OpeningDiff> {
    let text = |key: &str| {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .with_context(|| format!("openDiff needs a string `{key}`"))
    };
    let old_path = PathBuf::from(text("old_file_path")?);
    let new_path = PathBuf::from(text("new_file_path")?);
    let new_contents = text("new_file_contents")?.to_string();
    let tab_name = text("tab_name")?.to_string();
    let (window, workspace) = workspace_for(&new_path, cx)
        .context("Wu has no window with a project on this computer to show the diff in.")?;
    window.update(cx, |_, window, cx| {
        chat_view::open_diff_review(
            old_path,
            new_path,
            new_contents,
            tab_name,
            workspace.downgrade(),
            window,
            cx,
        )
    })
}

/// The workspace whose project holds `path`, else the active window's. Only a local project can show the diff.
fn workspace_for(
    path: &Path,
    cx: &App,
) -> Option<(WindowHandle<MultiWorkspace>, Entity<Workspace>)> {
    let active_window = cx.active_window().map(|window| window.window_id());
    let mut workspaces = cx
        .windows()
        .into_iter()
        .filter_map(|window| window.downcast::<MultiWorkspace>())
        .filter_map(|window| Some((window, window.read(cx).ok()?.workspace().clone())))
        .filter(|(_, workspace)| workspace.read(cx).project().read(cx).is_local())
        .collect::<Vec<_>>();
    workspaces.sort_by_key(|(window, _)| Some(window.window_id()) != active_window);
    workspaces
        .iter()
        .find(|(_, workspace)| {
            let project = workspace.read(cx).project().read(cx);
            project.find_project_path(path, cx).is_some()
        })
        .or(workspaces.first())
        .cloned()
}

/// Like `diagnostics_json`, over every local project open in wu.
fn all_diagnostics(uri: Option<&str>, minimum_severity: Option<&str>, cx: &App) -> Value {
    let mut projects = cx
        .windows()
        .into_iter()
        .filter_map(|window| window.downcast::<MultiWorkspace>())
        .filter_map(|window| window.read(cx).ok())
        .flat_map(|multi| multi.workspaces())
        .map(|workspace| workspace.read(cx).project().clone())
        .filter(|project| project.read(cx).is_local())
        .collect::<Vec<_>>();
    projects.dedup_by_key(|project| project.entity_id());
    if let Some(uri) = uri {
        let path = uri_path(uri);
        return projects
            .iter()
            .find(|project| project.read(cx).find_project_path(&path, cx).is_some())
            .map(|project| diagnostics_json(project, Some(uri), minimum_severity, cx))
            .unwrap_or_else(|| json!([{ "uri": uri, "diagnostics": [] }]));
    }
    let files = projects
        .iter()
        .flat_map(
            |project| match diagnostics_json(project, None, minimum_severity, cx) {
                Value::Array(files) => files,
                _ => Vec::new(),
            },
        )
        .collect();
    Value::Array(files)
}

/// The tools a panel chat gets from its own project.
pub(crate) struct PanelTools<'a> {
    pub project: Option<Entity<Project>>,
    pub cx: &'a App,
}

impl IdeTools for PanelTools<'_> {
    fn call(
        &self,
        name: &str,
        arguments: &Value,
    ) -> BoxFuture<'static, anyhow::Result<Vec<String>>> {
        let result = (|| -> anyhow::Result<Vec<String>> {
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
        })();
        futures::future::ready(result).boxed()
    }

    fn list(&self) -> Value {
        json!([diagnostics_tool()])
    }
}

fn diagnostics_tool() -> Value {
    json!({
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
    })
}

fn uri_path(uri: &str) -> PathBuf {
    lsp::Uri::from_str(uri)
        .ok()
        .and_then(|uri| uri.to_file_path().ok())
        .unwrap_or_else(|| PathBuf::from(uri))
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
        let project_path = project.find_project_path(&uri_path(uri), cx);
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
    use gpui::{TestAppContext, UpdateGlobal as _};
    use language::{DiagnosticEntry, DiagnosticMessage, DiagnosticSourceKind, Unclipped};
    use lsp::LanguageServerId;
    use project::FakeFs;
    use settings::SettingsStore;
    use util::path;

    struct FakeTools;

    impl IdeTools for FakeTools {
        fn call(&self, name: &str, _: &Value) -> BoxFuture<'static, anyhow::Result<Vec<String>>> {
            let result = if name == "getDiagnostics" {
                Ok(vec!["[]".to_string()])
            } else {
                Err(anyhow!("Unknown tool: {name}"))
            };
            futures::future::ready(result).boxed()
        }

        fn list(&self) -> Value {
            json!([{ "name": "getDiagnostics" }])
        }
    }

    fn request(id: u64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    fn handle_now(message: &Value) -> Value {
        handle_mcp_message(message, &FakeTools)
            .now_or_never()
            .expect("FakeTools answer at once")
    }

    #[test]
    fn handle_mcp_message_initialize_lists_and_calls() {
        let initialize = handle_now(&request(
            0,
            "initialize",
            json!({ "protocolVersion": "2025-11-25" }),
        ));
        assert_eq!(initialize["id"], 0);
        assert_eq!(initialize["result"]["protocolVersion"], "2025-11-25");
        assert!(initialize["result"]["capabilities"]["tools"].is_object());

        let list = handle_now(&request(1, "tools/list", json!({})));
        assert_eq!(list["result"]["tools"][0]["name"], "getDiagnostics");
        assert_eq!(list["result"]["tools"].as_array().map(Vec::len), Some(1));

        let call = handle_now(&request(
            2,
            "tools/call",
            json!({ "name": "getDiagnostics", "arguments": {} }),
        ));
        assert_eq!(call["id"], 2);
        assert_eq!(call["result"]["content"][0]["type"], "text");
        assert_eq!(call["result"]["content"][0]["text"], "[]");

        let notification = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert_eq!(
            handle_now(&notification),
            json!({ "jsonrpc": "2.0", "result": {}, "id": 0 })
        );

        let unknown = handle_now(&request(3, "tools/call", json!({ "name": "openDiff" })));
        assert_eq!(unknown["id"], 3);
        assert!(unknown["error"]["message"].is_string(), "{unknown}");

        for malformed in [
            json!(null),
            json!("text"),
            json!({ "id": 4 }),
            json!({ "id": 5, "method": 7 }),
        ] {
            assert!(handle_now(&malformed).is_object());
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
        start_server_with(Arc::new(FakeTools), cx)
    }

    fn start_server_with(
        tools: Arc<dyn IdeTools + Send + Sync>,
        cx: &mut TestAppContext,
    ) -> (IdeServer, tempfile::TempDir) {
        cx.executor().allow_parking();
        let directory = tempfile::tempdir().expect("temp dir");
        let server = cx
            .update(|cx| {
                IdeServer::start_in(
                    directory.path(),
                    vec![PathBuf::from(path!("/root"))],
                    tools,
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

    pub(super) struct TestLockDirectory(pub PathBuf);

    impl Global for TestLockDirectory {}

    #[test]
    fn lock_file_is_replaced_whole_and_owner_only() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("10001.lock");
        std::fs::write(&path, "left by an earlier run").expect("write lock file");
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o644))
            .expect("loosen permissions");

        write_lock_file(&path, &[PathBuf::from(path!("/root"))], "token").expect("written");

        let lock: Value = serde_json::from_slice(&std::fs::read(&path).expect("lock file"))
            .expect("lock file is JSON");
        assert_eq!(lock["authToken"], "token");
        let names = std::fs::read_dir(directory.path())
            .expect("directory is readable")
            .map(|entry| entry.expect("entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, ["10001.lock"], "no temporary file is left behind");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn at_mention_lines_are_zero_based() {
        let path = PathBuf::from(path!("/root/a.rs"));
        assert_eq!(
            at_mention_json(&path, Some((3, 5))),
            json!({ "filePath": path!("/root/a.rs"), "lineStart": 2, "lineEnd": 4 })
        );
        assert_eq!(
            at_mention_json(&path, None),
            json!({ "filePath": path!("/root/a.rs") })
        );
    }

    /// `wait` answers only once the test sends on `release`.
    struct WaitingTools {
        release: Mutex<Option<oneshot::Receiver<()>>>,
    }

    impl IdeTools for WaitingTools {
        fn call(&self, name: &str, _: &Value) -> BoxFuture<'static, anyhow::Result<Vec<String>>> {
            let release = lock(&self.release).take();
            let name = name.to_string();
            async move {
                anyhow::ensure!(name == "wait", "Unknown tool: {name}");
                release.context("wait was called twice")?.await?;
                Ok(vec!["waited".to_string()])
            }
            .boxed()
        }

        fn list(&self) -> Value {
            json!([{ "name": "wait" }])
        }
    }

    #[gpui::test]
    async fn requests_are_answered_while_a_tool_waits(cx: &mut TestAppContext) {
        let (release, released) = oneshot::channel();
        let tools = WaitingTools {
            release: Mutex::new(Some(released)),
        };
        let (server, _directory) = start_server_with(Arc::new(tools), cx);
        let mut client = connect(server.port, Some(&server.auth_token)).await;
        client
            .send(Message::text(
                request(1, "tools/call", json!({ "name": "wait", "arguments": {} })).to_string(),
            ))
            .await
            .expect("send");

        assert_eq!(list_tools(&mut client, 2).await["id"], 2);

        release.send(()).expect("the tool still waits");
        let waited = next_json(&mut client).await;
        assert_eq!(waited["id"], 1);
        assert_eq!(waited["result"]["content"][0]["text"], "waited");
    }

    fn set_agent_chat_enabled(enabled: bool, cx: &mut TestAppContext) {
        cx.update(|cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |content| {
                    content.agent_chat.get_or_insert_default().enabled = Some(enabled);
                })
            })
        });
        cx.run_until_parked();
    }

    fn lock_files(directory: &Path) -> usize {
        std::fs::read_dir(directory)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|entry| entry.path().extension() == Some("lock".as_ref()))
                    .count()
            })
            .unwrap_or(0)
    }

    #[gpui::test]
    async fn the_server_runs_only_while_agent_chat_is_on(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temp dir");
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
            cx.set_global(TestLockDirectory(directory.path().to_path_buf()));
        });
        set_agent_chat_enabled(false, cx);
        cx.update(|cx| crate::init(None, cx));
        cx.run_until_parked();
        assert_eq!(lock_files(directory.path()), 0);
        assert!(!cx.update(|cx| cx.has_global::<GlobalIdeServer>()));

        set_agent_chat_enabled(true, cx);
        assert_eq!(lock_files(directory.path()), 1);
        let port = cx.update(|cx| cx.global::<GlobalIdeServer>().server.port);
        assert_eq!(
            cx.read(terminal_env),
            [("CLAUDE_CODE_SSE_PORT".to_string(), port.to_string())]
        );

        set_agent_chat_enabled(false, cx);
        assert_eq!(lock_files(directory.path()), 0);
        assert!(cx.read(terminal_env).is_empty());
    }

    #[gpui::test]
    async fn selection_changed_is_debounced_and_zero_based(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temp dir");
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
            cx.set_global(TestLockDirectory(directory.path().to_path_buf()));
            crate::init(None, cx);
        });
        let (sender, mut sent) = mpsc::unbounded();
        cx.update(|cx| *lock(&cx.global::<GlobalIdeServer>().server.client) = Some(sender));
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({ "a.rs": "fn main() {\n    let é = 1;\n}\n" }),
        )
        .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        let editor = workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_abs_path(
                    PathBuf::from(path!("/root/a.rs")),
                    workspace::OpenOptions::default(),
                    window,
                    cx,
                )
            })
            .await
            .expect("the file opens")
            .downcast::<Editor>()
            .expect("an editor");
        cx.update(|window, cx| window.focus(&editor.focus_handle(cx), cx));
        cx.run_until_parked();
        while sent.try_recv().is_ok() {}

        for range in [
            language::Point::new(0, 0)..language::Point::new(0, 2),
            language::Point::new(1, 4)..language::Point::new(1, 7),
            language::Point::new(1, 8)..language::Point::new(1, 10),
        ] {
            editor.update_in(cx, |editor, window, cx| {
                editor.change_selections(Default::default(), window, cx, |selections| {
                    selections.select_ranges([range])
                })
            });
        }
        cx.executor()
            .advance_clock(SELECTION_DEBOUNCE - Duration::from_millis(1));
        cx.run_until_parked();
        assert!(sent.try_recv().is_err(), "nothing is sent before 300 ms");

        cx.executor().advance_clock(Duration::from_millis(1));
        cx.run_until_parked();
        let Ok(Message::Text(text)) = sent.try_recv() else {
            panic!("one notification after 300 ms");
        };
        assert!(sent.try_recv().is_err(), "only the last selection is sent");
        let notification: Value = serde_json::from_str(&text).expect("JSON");
        assert_eq!(notification["method"], "selection_changed");
        let params = &notification["params"];
        assert_eq!(params["text"], "é");
        assert_eq!(params["filePath"], path!("/root/a.rs"));
        assert!(
            params["fileUrl"]
                .as_str()
                .is_some_and(|url| url.starts_with("file://"))
        );
        assert_eq!(
            params["selection"],
            json!({
                "start": { "line": 1, "character": 8 },
                "end": { "line": 1, "character": 9 },
                "isEmpty": false,
            })
        );
    }

    struct DiffSetup<'a> {
        cx: &'a mut gpui::VisualTestContext,
        workspace: Entity<Workspace>,
        tools: Arc<TerminalTools>,
        _answering: Task<()>,
    }

    async fn diff_setup(cx: &mut TestAppContext) -> DiffSetup<'_> {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
            crate::init(None, cx);
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/project"),
            json!({ "a.rs": "one\n", "b.rs": "uno\n" }),
        )
        .await;
        let project = Project::test(fs, [path!("/project").as_ref()], cx).await;
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        let (tools, answering) = cx.update(|_, cx| terminal_tools(cx));
        DiffSetup {
            cx,
            workspace,
            tools,
            _answering: answering,
        }
    }

    fn call_tool(tools: &TerminalTools, name: &str, arguments: Value) -> BoxFuture<'static, Value> {
        let call = request(
            1,
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        handle_mcp_message(&call, tools)
    }

    fn open_diff_call(
        tools: &TerminalTools,
        file: &str,
        tab_name: &str,
    ) -> BoxFuture<'static, Value> {
        let path = format!("{}{file}", path!("/project/"));
        call_tool(
            tools,
            "openDiff",
            json!({
                "old_file_path": path,
                "new_file_path": path,
                "new_file_contents": "two\n",
                "tab_name": tab_name,
            }),
        )
    }

    fn texts(reply: Option<Value>) -> Vec<String> {
        let reply = reply.expect("the call was answered");
        reply["result"]["content"]
            .as_array()
            .unwrap_or_else(|| panic!("a tool result: {reply}"))
            .iter()
            .map(|block| {
                assert_eq!(block["type"], "text");
                block["text"].as_str().unwrap_or_default().to_string()
            })
            .collect()
    }

    fn diff_tabs(setup: &mut DiffSetup) -> Vec<Entity<ProposedEditReview>> {
        setup.workspace.read_with(setup.cx, |workspace, cx| {
            workspace.items_of_type::<ProposedEditReview>(cx).collect()
        })
    }

    fn focus_only_diff_tab(setup: &mut DiffSetup) -> Entity<ProposedEditReview> {
        let tabs = diff_tabs(setup);
        assert_eq!(tabs.len(), 1);
        let review = tabs[0].clone();
        setup
            .cx
            .update(|window, cx| window.focus(&review.focus_handle(cx), cx));
        setup.cx.run_until_parked();
        review
    }

    #[gpui::test]
    async fn open_diff_accept_returns_file_saved_and_contents(cx: &mut TestAppContext) {
        let mut setup = diff_setup(cx).await;
        let mut opening = open_diff_call(&setup.tools, "a.rs", "✻ [Claude Code] a.rs ⧉");
        setup.cx.run_until_parked();
        assert!(
            (&mut opening).now_or_never().is_none(),
            "openDiff waits for the user"
        );
        let review = focus_only_diff_tab(&mut setup);
        let title = setup
            .cx
            .read(|cx| workspace::item::Item::tab_content_text(review.read(cx), 0, cx));
        assert_eq!(title, "✻ [Claude Code] a.rs ⧉");

        setup.cx.simulate_input("x");
        setup.cx.dispatch_action(crate::AcceptProposedChanges);
        setup.cx.run_until_parked();

        assert_eq!(texts(opening.now_or_never()), ["FILE_SAVED", "xtwo\n"]);
        assert!(
            diff_tabs(&mut setup).is_empty(),
            "the tab closes once answered"
        );
    }

    #[gpui::test]
    async fn open_diff_reject_and_tab_close_return_diff_rejected(cx: &mut TestAppContext) {
        let mut setup = diff_setup(cx).await;

        let rejected = open_diff_call(&setup.tools, "a.rs", "rejected");
        setup.cx.run_until_parked();
        focus_only_diff_tab(&mut setup);
        setup.cx.dispatch_action(crate::RejectProposedChanges);
        setup.cx.run_until_parked();
        assert_eq!(
            texts(rejected.now_or_never()),
            ["DIFF_REJECTED", "rejected"]
        );
        assert!(diff_tabs(&mut setup).is_empty());

        let closed = open_diff_call(&setup.tools, "a.rs", "closed by the user");
        setup.cx.run_until_parked();
        focus_only_diff_tab(&mut setup);
        setup
            .workspace
            .update_in(setup.cx, |workspace, window, cx| {
                workspace.active_pane().update(cx, |pane, cx| {
                    pane.close_active_item(&Default::default(), window, cx)
                })
            })
            .await
            .expect("the tab closes");
        setup.cx.run_until_parked();
        assert_eq!(
            texts(closed.now_or_never()),
            ["DIFF_REJECTED", "closed by the user"]
        );

        let closed_by_claude = open_diff_call(&setup.tools, "b.rs", "closed by claude");
        setup.cx.run_until_parked();
        let close_tab = call_tool(
            &setup.tools,
            "close_tab",
            json!({ "tab_name": "closed by claude" }),
        );
        setup.cx.run_until_parked();
        assert_eq!(texts(close_tab.now_or_never()), ["TAB_CLOSED"]);
        assert_eq!(
            texts(closed_by_claude.now_or_never()),
            ["DIFF_REJECTED", "closed by claude"]
        );
        assert!(diff_tabs(&mut setup).is_empty());

        let abandoned = open_diff_call(&setup.tools, "a.rs", "abandoned");
        setup.cx.run_until_parked();
        assert_eq!(diff_tabs(&mut setup).len(), 1);
        drop(abandoned);
        setup.cx.run_until_parked();
        assert!(
            diff_tabs(&mut setup).is_empty(),
            "the tab closes once claude stops waiting"
        );
    }

    #[gpui::test]
    async fn close_all_diff_tabs_reports_count(cx: &mut TestAppContext) {
        let mut setup = diff_setup(cx).await;
        let first = open_diff_call(&setup.tools, "a.rs", "first");
        let second = open_diff_call(&setup.tools, "b.rs", "second");
        setup.cx.run_until_parked();
        assert_eq!(diff_tabs(&mut setup).len(), 2);

        let close_all = call_tool(&setup.tools, "closeAllDiffTabs", json!({}));
        setup.cx.run_until_parked();

        assert_eq!(texts(close_all.now_or_never()), ["CLOSED_2_DIFF_TABS"]);
        assert_eq!(texts(first.now_or_never()), ["DIFF_REJECTED", "first"]);
        assert_eq!(texts(second.now_or_never()), ["DIFF_REJECTED", "second"]);
        assert!(diff_tabs(&mut setup).is_empty());
    }
}
