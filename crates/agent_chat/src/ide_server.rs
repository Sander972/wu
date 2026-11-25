use std::ops::Range;
use std::path::PathBuf;
use std::str::FromStr as _;

use anyhow::Context as _;
use gpui::{App, Entity};
use language::{Diagnostic, PointUtf16};
use lsp::{DiagnosticSeverity, NumberOrString};
use project::{Project, ProjectPath};
use serde_json::{Value, json};

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
