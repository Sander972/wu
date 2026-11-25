use agent_harness::PermissionDecision;
use anyhow::{Result, anyhow};
use serde_json::{Value, json};

const REVERTED_ALL: &str = "User reverted all proposed changes in the diff view";
const STALE_ON_DISK: &str =
    "The file changed on disk while the edit was being reviewed. Read it again before editing.";

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
