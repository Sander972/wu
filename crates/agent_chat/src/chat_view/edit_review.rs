use crate::{
    AcceptChangeAtCursor, AcceptProposedChanges, RejectChangeAtCursor, RejectProposedChanges,
    session::AgentSession,
};
use agent_harness::{PermissionDecision, PermissionPrompt};
use anyhow::{Context as _, Result, anyhow};
use buffer_diff::DiffHunkStatus;
use collections::HashSet;
use editor::{Anchor, DiffHunkRenderer, Editor, SplittableEditor};
use fs::Fs;
use git_ui_core::file_diff_view::{FileDiffView, build_buffer_diff};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, Render, SharedString, Subscription, Task, WeakEntity, Window,
};
use i18n::t;
use language::{Buffer, LineEnding};
use serde_json::{Value, json};
use std::{
    any::TypeId,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};
use theme::ActiveTheme as _;
use ui::{Button, ButtonStyle, Icon, IconName, prelude::*};
use util::ResultExt as _;
use workspace::{
    Workspace,
    item::{Item, ItemEvent},
    notifications::NotifyTaskExt as _,
};

const REVERTED_ALL: &str = "User reverted all proposed changes in the diff view";
const CANCEL_EDIT: &str = "User cancelled the edit";
const CANCEL_WRITE: &str = "User cancelled the write";
const STALE_ON_DISK: &str =
    "The file changed on disk while the edit was being reviewed. Read it again before editing.";
const MAX_HUNKS_WITH_BUTTONS: usize = 100;

pub(crate) fn is_reviewable(prompt: &PermissionPrompt) -> bool {
    matches!(prompt.tool_name.as_str(), "Edit" | "Write")
}

fn cancel_decision(tool_name: &str) -> PermissionDecision {
    let message = if tool_name == "Write" {
        CANCEL_WRITE
    } else {
        CANCEL_EDIT
    };
    PermissionDecision::Deny {
        message: message.into(),
        interrupt: true,
    }
}

/// Both the snapshot taken when the review opens and the re-read at Accept go through here, so a CRLF file is not mistaken for one that changed on disk.
async fn read_disk_text(fs: &dyn Fs, path: &Path) -> Result<String> {
    if !fs.is_file(path).await {
        return Ok(String::new());
    }
    let mut text = fs.load(path).await?;
    LineEnding::normalize(&mut text);
    Ok(text)
}

fn is_pending(session: &AgentSession, question_id: &str) -> bool {
    session
        .pending_questions()
        .iter()
        .any(|pending| pending.id() == question_id)
}

pub(crate) fn open_review(
    session: Entity<AgentSession>,
    question_id: String,
    prompt: &PermissionPrompt,
    focus: bool,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> Task<Result<()>> {
    let tool_name = prompt.tool_name.clone();
    let input = prompt.input.clone();
    window.spawn(cx, async move |cx| {
        let path = PathBuf::from(
            input["file_path"]
                .as_str()
                .context("the proposed change has no `file_path`")?,
        );
        let project = workspace.read_with(cx, |workspace, _| workspace.project().clone())?;
        let (fs, languages) = project.read_with(cx, |project, _| {
            (project.fs().clone(), project.languages().clone())
        });
        let original = read_disk_text(fs.as_ref(), &path).await?;
        let proposed = proposed_text(&tool_name, &input, &original)?;
        let language = languages.load_language_for_file_path(&path).await.ok();
        let (old_buffer, new_buffer) = project.update(cx, |project, cx| {
            (
                project.create_local_buffer(&original, language.clone(), false, cx),
                project.create_local_buffer(&proposed, language, false, cx),
            )
        });
        let diff = build_buffer_diff(&old_buffer, &new_buffer, cx).await?;
        workspace.update_in(cx, |workspace, window, cx| {
            if !is_pending(session.read(cx), &question_id) {
                return Ok(());
            }
            let open = workspace
                .items_of_type::<ProposedEditReview>(cx)
                .find(|review| review.read(cx).question_id == question_id);
            if let Some(review) = open {
                workspace.activate_item(&review, true, focus, window, cx);
                return Ok(());
            }
            let workspace_entity = cx.entity();
            let weak_workspace = workspace_entity.downgrade();
            let diff_view = cx.new(|cx| {
                FileDiffView::new(
                    old_buffer,
                    new_buffer.clone(),
                    diff,
                    project,
                    workspace_entity,
                    window,
                    cx,
                )
            });
            let splittable = diff_view
                .read(cx)
                .act_as_type(TypeId::of::<SplittableEditor>(), &diff_view, cx)
                .and_then(|editor| editor.downcast::<SplittableEditor>().ok())
                .context("the diff view has no editor")?;
            let file_name = path
                .file_name()
                .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy())
                .into_owned();
            let review = cx.new(|cx| {
                let renderer = Arc::new(ProposedHunkRenderer {
                    review: cx.weak_entity(),
                });
                splittable.update(cx, |splittable, cx| {
                    splittable.set_diff_hunk_renderer(Some(renderer), cx)
                });
                ProposedEditReview {
                    _subscriptions: vec![
                        cx.observe(&session, |this: &mut ProposedEditReview, session, cx| {
                            if !is_pending(session.read(cx), &this.question_id) {
                                cx.emit(ItemEvent::CloseItem);
                            }
                        }),
                        // Closing the tab without answering must not leave the agent waiting.
                        cx.on_release(|this: &mut ProposedEditReview, cx| {
                            this.decide(cancel_decision(&this.tool_name), cx)
                        }),
                    ],
                    title: t!("{file} (proposed)", file = file_name),
                    session,
                    question_id,
                    tool_name,
                    input,
                    path,
                    original,
                    proposed,
                    new_buffer,
                    diff_view,
                    splittable,
                    decided_hunks: HashSet::default(),
                    fs,
                    workspace: weak_workspace,
                }
            });
            // An auto-opened review must neither take focus nor cover the chat tab being typed in (the pane would then hand focus to the review): keys meant for the chat would become a hand-edit that Accept sends.
            let pane = workspace.active_pane().clone();
            let activate = focus || !pane.focus_handle(cx).contains_focused(window, cx);
            pane.update(cx, |pane, cx| {
                pane.add_item_inner(Box::new(review), false, focus, activate, None, window, cx)
            });
            Ok(())
        })?
    })
}

pub(crate) struct ProposedEditReview {
    session: Entity<AgentSession>,
    question_id: String,
    tool_name: String,
    input: Value,
    path: PathBuf,
    original: String,
    proposed: String,
    new_buffer: Entity<Buffer>,
    diff_view: Entity<FileDiffView>,
    splittable: Entity<SplittableEditor>,
    /// Hunks are keyed by the whole range of original text they replace (it stays fixed while the proposal is edited), so a hunk that a hand-edit later merges with a neighbor counts as undecided again.
    decided_hunks: HashSet<Range<usize>>,
    fs: Arc<dyn Fs>,
    workspace: WeakEntity<Workspace>,
    title: SharedString,
    _subscriptions: Vec<Subscription>,
}

impl ProposedEditReview {
    fn accept(&mut self, _: &AcceptProposedChanges, window: &mut Window, cx: &mut Context<Self>) {
        let final_text = self.new_buffer.read(cx).text();
        let fs = self.fs.clone();
        let path = self.path.clone();
        cx.spawn(async move |this, cx| {
            let disk_text = read_disk_text(fs.as_ref(), &path).await?;
            this.update(cx, |this, cx| {
                let decision = review_decision(
                    &this.tool_name,
                    &this.input,
                    &this.original,
                    &this.proposed,
                    &final_text,
                    &disk_text,
                );
                this.decide(decision, cx);
            })
        })
        .detach_and_notify_err(self.workspace.clone(), window, cx);
    }

    fn reject(&mut self, _: &RejectProposedChanges, _: &mut Window, cx: &mut Context<Self>) {
        self.decide(cancel_decision(&self.tool_name), cx);
    }

    fn accept_change_at_cursor(
        &mut self,
        _: &AcceptChangeAtCursor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.decide_change_at_cursor(true, window, cx);
    }

    fn reject_change_at_cursor(
        &mut self,
        _: &RejectChangeAtCursor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.decide_change_at_cursor(false, window, cx);
    }

    fn decide_change_at_cursor(
        &mut self,
        accept: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = self.splittable.read(cx).focused_editor().clone();
        let cursor = editor.read(cx).selections.newest_anchor().head();
        self.decide_hunk(accept, &editor, cursor..cursor, window, cx);
    }

    fn decide_hunk(
        &mut self,
        accept: bool,
        editor: &Entity<Editor>,
        range: Range<Anchor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hunk = {
            let editor = editor.read(cx);
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            editor.diff_hunks_in_ranges(&[range], &snapshot).next()
        };
        let Some(hunk) = hunk else {
            return;
        };
        self.decided_hunks
            .insert(hunk.diff_base_byte_range.start.0..hunk.diff_base_byte_range.end.0);
        if !accept {
            editor.update(cx, |editor, cx| {
                editor.apply_restore(vec![hunk], window, cx)
            });
        }
        // A rejected hunk stays in the diff until it is recomputed, but it is already in `decided_hunks`.
        let all_decided = self
            .splittable
            .read(cx)
            .rhs_editor()
            .read(cx)
            .buffer()
            .read(cx)
            .snapshot(cx)
            .diff_hunks()
            .all(|hunk| {
                self.decided_hunks
                    .contains(&(hunk.diff_base_byte_range.start.0..hunk.diff_base_byte_range.end.0))
            });
        if all_decided {
            self.accept(&AcceptProposedChanges, window, cx);
        }
    }

    fn decide(&self, decision: PermissionDecision, cx: &mut App) {
        self.session.update(cx, |session, cx| {
            session.decide_permission(&self.question_id, decision, cx)
        });
    }
}

impl EventEmitter<ItemEvent> for ProposedEditReview {}

impl Focusable for ProposedEditReview {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.diff_view.focus_handle(cx)
    }
}

impl Item for ProposedEditReview {
    type Event = ItemEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        self.title.clone()
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Diff).color(Color::Muted))
    }

    fn tab_tooltip_text(&self, _cx: &App) -> Option<SharedString> {
        Some(self.path.to_string_lossy().into_owned().into())
    }

    fn to_item_events(event: &ItemEvent, f: &mut dyn FnMut(ItemEvent)) {
        f(*event)
    }
}

impl Render for ProposedEditReview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("ProposedEditReview")
            .size_full()
            .on_action(cx.listener(Self::accept))
            .on_action(cx.listener(Self::reject))
            .on_action(cx.listener(Self::accept_change_at_cursor))
            .on_action(cx.listener(Self::reject_change_at_cursor))
            .child(
                h_flex()
                    .w_full()
                    .p_1()
                    .gap_1()
                    .justify_end()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        Button::new("reject-proposed-changes", t!("Reject")).on_click(cx.listener(
                            |this, _, window, cx| this.reject(&RejectProposedChanges, window, cx),
                        )),
                    )
                    .child(
                        Button::new("accept-proposed-changes", t!("Accept"))
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.accept(&AcceptProposedChanges, window, cx)
                            })),
                    ),
            )
            .child(self.diff_view.clone())
    }
}

struct ProposedHunkRenderer {
    review: WeakEntity<ProposedEditReview>,
}

impl DiffHunkRenderer for ProposedHunkRenderer {
    fn render_hunk_controls(
        &self,
        row: u32,
        _status: &DiffHunkStatus,
        hunk_range: Range<Anchor>,
        _is_created_file: bool,
        line_height: Pixels,
        editor: &Entity<Editor>,
        _window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let too_many_hunks = editor
            .read(cx)
            .buffer()
            .read(cx)
            .snapshot(cx)
            .diff_hunks()
            .nth(MAX_HUNKS_WITH_BUTTONS)
            .is_some();
        if too_many_hunks {
            return gpui::Empty.into_any_element();
        }
        let button = |id: &'static str, label: SharedString, accept: bool| {
            let review = self.review.clone();
            let editor = editor.clone();
            let hunk_start = hunk_range.start;
            Button::new((id, row as u64), label).on_click(move |_, window, cx| {
                review
                    .update(cx, |review, cx| {
                        review.decide_hunk(accept, &editor, hunk_start..hunk_start, window, cx)
                    })
                    .log_err();
            })
        };
        h_flex()
            .debug_selector(|| "proposed-hunk-buttons".into())
            .h(line_height)
            .mr_1()
            .gap_1()
            .px_0p5()
            .pb_1()
            .border_x_1()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .rounded_b_lg()
            .bg(cx.theme().colors().editor_background)
            .block_mouse_except_scroll()
            .shadow_md()
            .child(button("reject-change", t!("Reject this change"), false))
            .child(button("accept-change", t!("Accept this change"), true))
            .into_any_element()
    }

    fn render_hunk_as_staged(&self, _status: &DiffHunkStatus, _cx: &App) -> bool {
        false
    }
}

pub(crate) fn proposed_text(tool_name: &str, input: &Value, original: &str) -> Result<String> {
    if tool_name == "Write" {
        return input["content"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("Write input has no string `content`"));
    }
    let old_string = input["old_string"]
        .as_str()
        .ok_or_else(|| anyhow!("Edit input has no string `old_string`"))?;
    let new_string = input["new_string"]
        .as_str()
        .ok_or_else(|| anyhow!("Edit input has no string `new_string`"))?;
    if !original.contains(old_string) {
        return Err(anyhow!("`old_string` was not found in the file"));
    }
    if input["replace_all"].as_bool().unwrap_or(false) {
        Ok(original.replace(old_string, new_string))
    } else {
        Ok(original.replacen(old_string, new_string, 1))
    }
}

pub(crate) fn review_decision(
    tool_name: &str,
    input: &Value,
    original: &str,
    proposed: &str,
    final_text: &str,
    disk_text: &str,
) -> PermissionDecision {
    if disk_text != original {
        return PermissionDecision::Deny {
            message: STALE_ON_DISK.into(),
            interrupt: false,
        };
    }
    if final_text == original {
        return PermissionDecision::Deny {
            message: REVERTED_ALL.into(),
            interrupt: true,
        };
    }
    let updated_input = if final_text == proposed {
        None
    } else if tool_name == "Write" {
        Some(json!({ "file_path": input["file_path"], "content": final_text }))
    } else {
        Some(json!({
            "file_path": input["file_path"],
            "old_string": original,
            "new_string": final_text,
            "replace_all": false,
        }))
    };
    PermissionDecision::Allow {
        updated_input,
        updated_permissions: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(old: &str, new: &str, replace_all: bool) -> Value {
        json!({"file_path": "/a.txt", "old_string": old, "new_string": new, "replace_all": replace_all})
    }

    #[test]
    fn edit_applies_first_match_only() {
        let text = proposed_text("Edit", &edit("a", "b", false), "a a a").unwrap();
        assert_eq!(text, "b a a");
    }

    #[test]
    fn edit_replace_all() {
        let text = proposed_text("Edit", &edit("a", "b", true), "a a a").unwrap();
        assert_eq!(text, "b b b");
    }

    #[test]
    fn edit_missing_old_string_errors() {
        assert!(proposed_text("Edit", &edit("zzz", "b", false), "a a a").is_err());
    }

    #[test]
    fn write_uses_content() {
        let input = json!({"file_path": "/a.txt", "content": "new"});
        assert_eq!(proposed_text("Write", &input, "old").unwrap(), "new");
    }

    #[test]
    fn unchanged_proposal_allows_original_input() {
        let input = edit("a", "b", false);
        let decision = review_decision("Edit", &input, "a", "b", "b", "a");
        assert_eq!(
            decision,
            PermissionDecision::Allow {
                updated_input: None,
                updated_permissions: vec![]
            }
        );
    }

    #[test]
    fn user_edited_proposal_sends_whole_file_edit() {
        let input = edit("a", "b", false);
        let decision = review_decision("Edit", &input, "a", "b", "c", "a");
        let PermissionDecision::Allow {
            updated_input: Some(updated),
            ..
        } = decision
        else {
            panic!("expected allow with updated input");
        };
        assert_eq!(
            updated,
            json!({"file_path": "/a.txt", "old_string": "a", "new_string": "c", "replace_all": false})
        );
        assert_eq!(updated.as_object().map(|map| map.len()), Some(4));

        let write = json!({"file_path": "/a.txt", "content": "b"});
        let decision = review_decision("Write", &write, "a", "b", "c", "a");
        assert_eq!(
            decision,
            PermissionDecision::Allow {
                updated_input: Some(json!({"file_path": "/a.txt", "content": "c"})),
                updated_permissions: vec![]
            }
        );
    }

    #[test]
    fn reverting_everything_denies_with_reverted_message() {
        let decision = review_decision("Edit", &edit("a", "b", false), "a", "b", "a", "a");
        assert_eq!(
            decision,
            PermissionDecision::Deny {
                message: REVERTED_ALL.into(),
                interrupt: true
            }
        );
    }

    struct ReviewSetup {
        workspace: Entity<Workspace>,
        session: Entity<AgentSession>,
        store: Entity<crate::session::AgentStore>,
        project: Entity<project::Project>,
        prompt: PermissionPrompt,
        answers: tokio::sync::oneshot::Receiver<Vec<agent_harness::UserInputAnswer>>,
        _directory: tempfile::TempDir,
    }

    async fn open_edit_review<'a>(
        file_text: &str,
        old_string: &str,
        new_string: &str,
        cx: &'a mut gpui::TestAppContext,
    ) -> (ReviewSetup, &'a mut gpui::VisualTestContext) {
        let (setup, cx) = review_setup(file_text, old_string, new_string, cx).await;
        setup
            .workspace
            .update_in(cx, |workspace, window, cx| {
                open_review(
                    setup.session.clone(),
                    "q".into(),
                    &setup.prompt,
                    true,
                    workspace.weak_handle(),
                    window,
                    cx,
                )
            })
            .await
            .expect("the review opens");
        cx.run_until_parked();
        assert_eq!(reviews(&setup.workspace, cx), 1);
        (setup, cx)
    }

    /// Leaves an `Edit` of `a.rs` from `old_string` to `new_string` waiting for permission, with no review open yet.
    async fn review_setup<'a>(
        file_text: &str,
        old_string: &str,
        new_string: &str,
        cx: &'a mut gpui::TestAppContext,
    ) -> (ReviewSetup, &'a mut gpui::VisualTestContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
            crate::init(None, cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = crate::session::tests::new_store(directory.path(), cx);
        let fs = project::FakeFs::new(cx.executor());
        fs.insert_tree(util::path!("/project"), json!({ "a.rs": file_text }))
            .await;
        let project = project::Project::test(fs, [util::path!("/project").as_ref()], cx).await;
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        let session = store.update(cx, |store, cx| {
            store.create_session(crate::AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let prompt = PermissionPrompt {
            tool_name: "Edit".into(),
            input: json!({
                "file_path": util::path!("/project/a.rs"),
                "old_string": old_string,
                "new_string": new_string,
                "replace_all": false,
            }),
            ..PermissionPrompt::default()
        };
        let answers = session.update(cx, |session, _| {
            crate::session::tests::push_permission(session, "q", prompt.clone())
        });
        (
            ReviewSetup {
                workspace,
                session,
                store,
                project,
                prompt,
                answers,
                _directory: directory,
            },
            cx,
        )
    }

    fn reviews(workspace: &Entity<Workspace>, cx: &mut gpui::VisualTestContext) -> usize {
        workspace.read_with(cx, |workspace, cx| {
            workspace.items_of_type::<ProposedEditReview>(cx).count()
        })
    }

    fn decision(setup: &mut ReviewSetup) -> Option<PermissionDecision> {
        setup
            .answers
            .try_recv()
            .expect("the agent got an answer")
            .first()
            .and_then(|answer| answer.decision.clone())
    }

    #[gpui::test]
    async fn accepting_a_review_allows_the_edit(cx: &mut gpui::TestAppContext) {
        let (mut setup, cx) = open_edit_review("one\n", "one", "two", cx).await;
        cx.dispatch_action(AcceptProposedChanges);
        cx.run_until_parked();
        assert_eq!(
            decision(&mut setup),
            Some(PermissionDecision::Allow {
                updated_input: None,
                updated_permissions: vec![]
            })
        );
        assert_eq!(reviews(&setup.workspace, cx), 0);
    }

    #[gpui::test]
    async fn accepting_a_crlf_file_is_not_mistaken_for_a_change_on_disk(
        cx: &mut gpui::TestAppContext,
    ) {
        let (mut setup, cx) = open_edit_review("one\r\nkeep\r\n", "one", "two", cx).await;
        cx.dispatch_action(AcceptProposedChanges);
        cx.run_until_parked();
        assert_eq!(
            decision(&mut setup),
            Some(PermissionDecision::Allow {
                updated_input: None,
                updated_permissions: vec![]
            })
        );
    }

    #[gpui::test]
    async fn rejecting_a_review_denies_with_cancel_message(cx: &mut gpui::TestAppContext) {
        let (mut setup, cx) = open_edit_review("one\n", "one", "two", cx).await;
        cx.dispatch_action(RejectProposedChanges);
        cx.run_until_parked();
        assert_eq!(
            decision(&mut setup),
            Some(PermissionDecision::Deny {
                message: CANCEL_EDIT.into(),
                interrupt: true
            })
        );
        assert_eq!(reviews(&setup.workspace, cx), 0);
    }

    #[gpui::test]
    async fn closing_the_review_tab_rejects(cx: &mut gpui::TestAppContext) {
        let (mut setup, cx) = open_edit_review("one\n", "one", "two", cx).await;
        setup
            .workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.active_pane().update(cx, |pane, cx| {
                    pane.close_active_item(&Default::default(), window, cx)
                })
            })
            .await
            .expect("the tab closes");
        cx.run_until_parked();
        assert_eq!(
            decision(&mut setup),
            Some(PermissionDecision::Deny {
                message: CANCEL_EDIT.into(),
                interrupt: true
            })
        );
    }

    #[gpui::test]
    async fn ask_mode_opens_the_review_automatically(cx: &mut gpui::TestAppContext) {
        let (setup, cx) = review_setup("one\n", "one", "two", cx).await;
        setup.session.update(cx, |session, cx| {
            session.update_settings(
                |settings| settings.permission = agent_harness::PermissionMode::Ask,
                cx,
            )
        });
        setup.workspace.update_in(cx, |workspace, window, cx| {
            let chat = cx.new(|cx| {
                crate::ChatView::new(
                    setup.session.clone(),
                    setup.store.clone(),
                    setup.project.clone(),
                    workspace.weak_handle(),
                    window,
                    cx,
                )
            });
            workspace.add_item_to_active_pane(Box::new(chat), None, true, window, cx);
        });
        setup.session.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert_eq!(reviews(&setup.workspace, cx), 1);
        let review = only_review(&setup.workspace, cx);
        let review_focused =
            cx.update(|window, cx| review.focus_handle(cx).contains_focused(window, cx));
        assert!(
            !review_focused,
            "an automatically opened review must not take keystrokes meant for the chat"
        );
    }

    fn only_review(
        workspace: &Entity<Workspace>,
        cx: &mut gpui::VisualTestContext,
    ) -> Entity<ProposedEditReview> {
        workspace.read_with(cx, |workspace, cx| {
            workspace
                .items_of_type::<ProposedEditReview>(cx)
                .next()
                .expect("a review is open")
        })
    }

    fn put_cursor_in_hunk(
        review: &Entity<ProposedEditReview>,
        index: usize,
        cx: &mut gpui::VisualTestContext,
    ) {
        let editor = review.read_with(cx, |review, cx| {
            review.splittable.read(cx).rhs_editor().clone()
        });
        editor.update_in(cx, |editor, window, cx| {
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            let start = snapshot
                .diff_hunks()
                .nth(index)
                .expect("the hunk exists")
                .multi_buffer_range
                .start;
            editor.change_selections(Default::default(), window, cx, |selections| {
                selections.select_anchor_ranges([start..start])
            });
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn rejecting_one_of_two_hunks_sends_the_remaining_change(cx: &mut gpui::TestAppContext) {
        let original = "one\nb\nc\nd\nfive\n";
        let (mut setup, cx) =
            open_edit_review(original, original, "ONE\nb\nc\nd\nFIVE\n", cx).await;
        let review = only_review(&setup.workspace, cx);
        put_cursor_in_hunk(&review, 1, cx);
        assert!(
            cx.debug_bounds("proposed-hunk-buttons").is_some(),
            "the hunk under the cursor shows its buttons"
        );
        cx.dispatch_action(RejectChangeAtCursor);
        put_cursor_in_hunk(&review, 0, cx);
        cx.dispatch_action(AcceptChangeAtCursor);
        cx.run_until_parked();
        let Some(PermissionDecision::Allow {
            updated_input: Some(updated),
            ..
        }) = decision(&mut setup)
        else {
            panic!("expected allow with updated input");
        };
        assert_eq!(updated["old_string"], original);
        assert_eq!(updated["new_string"], "ONE\nb\nc\nd\nfive\n");
        assert_eq!(reviews(&setup.workspace, cx), 0);
    }

    #[gpui::test]
    async fn more_than_100_hunks_shows_no_hunk_buttons(cx: &mut gpui::TestAppContext) {
        let original: String = (0..101)
            .map(|line| format!("line {line}\nkeep\n"))
            .collect();
        let proposed: String = (0..101)
            .map(|line| format!("LINE {line}\nkeep\n"))
            .collect();
        let (setup, cx) = open_edit_review(&original, &original, &proposed, cx).await;
        let review = only_review(&setup.workspace, cx);
        put_cursor_in_hunk(&review, 0, cx);
        assert!(cx.debug_bounds("proposed-hunk-buttons").is_none());
    }

    #[gpui::test]
    async fn rejecting_every_hunk_denies_as_reverted(cx: &mut gpui::TestAppContext) {
        let original = "one\nb\nc\nd\nfive\n";
        let (mut setup, cx) =
            open_edit_review(original, original, "ONE\nb\nc\nd\nFIVE\n", cx).await;
        let review = only_review(&setup.workspace, cx);
        put_cursor_in_hunk(&review, 1, cx);
        cx.dispatch_action(RejectChangeAtCursor);
        put_cursor_in_hunk(&review, 0, cx);
        cx.dispatch_action(RejectChangeAtCursor);
        cx.run_until_parked();
        assert_eq!(
            decision(&mut setup),
            Some(PermissionDecision::Deny {
                message: REVERTED_ALL.into(),
                interrupt: true
            })
        );
    }

    #[gpui::test]
    async fn a_hand_edit_that_merges_an_accepted_hunk_makes_it_undecided(
        cx: &mut gpui::TestAppContext,
    ) {
        let original = "a\nb\nc\nd\ne\nf\n";
        let (mut setup, cx) = open_edit_review(original, original, "A\nb\nC\nd\ne\nF\n", cx).await;
        let review = only_review(&setup.workspace, cx);
        put_cursor_in_hunk(&review, 0, cx);
        cx.dispatch_action(AcceptChangeAtCursor);
        let new_buffer = review.read_with(cx, |review, _| review.new_buffer.clone());
        new_buffer.update(cx, |buffer, cx| buffer.edit([(2..3, "B")], None, cx));
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        let hunk_count = review.read_with(cx, |review, cx| {
            review
                .splittable
                .read(cx)
                .rhs_editor()
                .read(cx)
                .buffer()
                .read(cx)
                .snapshot(cx)
                .diff_hunks()
                .count()
        });
        assert_eq!(hunk_count, 2, "the hand-edit merged the first two hunks");
        put_cursor_in_hunk(&review, 1, cx);
        cx.dispatch_action(AcceptChangeAtCursor);
        cx.run_until_parked();
        assert!(
            setup.answers.try_recv().is_err(),
            "the agent's undecided change must not be sent"
        );
        assert_eq!(reviews(&setup.workspace, cx), 1);
    }

    #[gpui::test]
    async fn answering_in_the_chat_closes_the_review(cx: &mut gpui::TestAppContext) {
        let (setup, cx) = open_edit_review("one\n", "one", "two", cx).await;
        setup
            .session
            .update(cx, |session, cx| session.answer_permission("q", true, cx));
        cx.run_until_parked();
        assert_eq!(reviews(&setup.workspace, cx), 0);
    }

    #[test]
    fn stale_original_denies_instead_of_overwriting() {
        let decision = review_decision("Edit", &edit("a", "b", false), "a", "b", "c", "changed");
        assert_eq!(
            decision,
            PermissionDecision::Deny {
                message: STALE_ON_DISK.into(),
                interrupt: false
            }
        );
    }
}
