#!/bin/sh
# Fake Claude Code CLI for agent_harness tests.
#
# Reads the first stream-json user line from stdin, picks a scenario from the
# prompt text, and plays a scripted stream-json transcript on stdout,
# including control-channel round-trips read back from stdin. Frame shapes
# mirror live captures from CLI 2.1.228. Driven by
# crates/harness/tests/claude.rs.

read -r first || exit 1

emit() { printf '%s\n' "$1"; }

# Permission mode changes can arrive between any two replies.
read_reply() {
  while read -r line; do
    case "$line" in
    *'"subtype":"set_permission_mode"'*) ;;
    *) eval "$1=\$line"; return 0 ;;
    esac
  done
  return 1
}

request_id() { printf '%s\n' "$1" | sed 's/.*"request_id":"\([^"]*\)".*/\1/'; }
text_of() { printf '%s\n' "$1" | sed 's/.*"content":"\([^"]*\)".*/\1/'; }
text_delta() { emit "{\"type\":\"stream_event\",\"parent_tool_use_id\":null,\"event\":{\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"$1\"}}}"; }

# Logged with the turn state ($2) so tests can tell which rewinds were sent, and whether a turn was running.
answer_rewind() {
  printf '%s %s\n' "$2" "$1" >> .rewind-requests
  case "$1" in
  *'"user_message_id":"no-checkpoint"'*)
    emit "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$(request_id "$1")\",\"response\":{\"canRewind\":false,\"error\":\"No checkpoint\"}}}"
    ;;
  *'"user_message_id":"rewind-error"'*)
    emit "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$(request_id "$1")\",\"response\":{\"canRewind\":true,\"error\":\"Disk full\",\"filesChanged\":[\"/repo/b.rs\"]}}}"
    ;;
  *'"user_message_id":"no-changes"'*)
    emit "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$(request_id "$1")\",\"response\":{\"canRewind\":true,\"filesChanged\":[],\"insertions\":0,\"deletions\":0}}}"
    ;;
  *)
    emit "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$(request_id "$1")\",\"response\":{\"canRewind\":true,\"filesChanged\":[\"/repo/src/a.rs\",\"/repo/b.rs\"],\"insertions\":3,\"deletions\":1}}}"
    ;;
  esac
}

# Shape mirrors 2.1.228's control_response: commands under response.response.
answer_initialize() {
  rid=$(request_id "$1")
  if [ -f .command-fixture ]; then
    name=$(cat .command-fixture)
    emit "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$rid\",\"response\":{\"commands\":[{\"name\":\"$name\"}]}}}"
  else
    emit "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$rid\",\"response\":{\"commands\":[{\"name\":\"review\",\"description\":\"Review a pull request\",\"argumentHint\":\"[pr number]\"},{\"name\":\"compact\",\"description\":\"Compact the conversation\",\"argumentHint\":\"\"},{\"name\":\"usage\",\"description\":\"Show plan usage\"},{\"name\":\"model\",\"description\":\"Set the AI model\"},{\"name\":\"\",\"description\":\"nameless: dropped\"}],\"output_style\":\"default\"}}}"
  fi
}

# A run asks for its commands ahead of the first prompt; a probe sends nothing else.
case "$first" in
*'"subtype":"initialize"'*)
  case " $* " in
  *' --include-partial-messages '*)
    answer_initialize "$first"
    init_line=$first
    read -r first || exit 1
    ;;
  esac
  ;;
esac

case "$first" in

*scenario:fork-fallback*)
  for arg in "$@"; do [ "$arg" = "--fork-session" ] && exit 1; done
  case "$first" in *'<conversation>'*) ;; *) exit 1 ;; esac
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-fallback"}'
  emit '{"type":"result","subtype":"success","result":"from the history","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-fallback"}'
  ;;

*scenario:fork*)
  forked=false
  resumed=false
  for arg in "$@"; do
    case "$arg" in
      --fork-session) forked=true ;;
      --resume=sess-1) resumed=true ;;
    esac
  done
  [ "$forked" = true ] && [ "$resumed" = true ] || exit 1
  case "$first" in *'<conversation>'*) exit 1 ;; esac
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-forked"}'
  emit '{"type":"result","subtype":"success","result":"forked","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-forked"}'
  ;;

*scenario:command-echo*)
  content=$(printf '%s\n' "$first" | sed 's/.*"content":"\([^"]*\)".*/\1/')
  emit "{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"$content\"}}}"
  emit '{"type":"result","subtype":"success","result":"echoed","usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-command-echo"}'
  ;;

*scenario:title*)
  tools_off=false
  system_set=false
  mcp_off=false
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --tools) shift; [ "$1" = "" ] && tools_off=true ;;
      --system-prompt) shift; case "$1" in "You generate session titles."*) system_set=true ;; esac ;;
      --strict-mcp-config) mcp_off=true ;;
      --dangerously-skip-permissions) exit 1 ;;
    esac
    shift
  done
  [ "$tools_off" = true ] && [ "$system_set" = true ] && [ "$mcp_off" = true ] || exit 1
  emit '{"type":"control_request","request_id":"title-tool","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"touch should-not-exist"}}}'
  read -r response || exit 1
  case "$response" in *'"behavior":"deny"'*) ;; *) exit 1 ;; esac
  emit '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Fix Login Flow"}}}'
  emit '{"type":"result","subtype":"success","result":"Fix Login Flow","usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-title"}'
  ;;


*scenario:happy*)
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Bash","Read"],"cwd":"/tmp","session_id":"sess-1"}'
  # Re-emitted init mid-run (background-task wakeup): must be deduped.
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Bash","Read"],"cwd":"/tmp","session_id":"sess-1"}'
  # Post-init system subtypes the 2.1.x CLI emits (thinking accounting,
  # session state): must be tolerated and dropped.
  emit '{"type":"system","subtype":"thinking_tokens","tokens":12}'
  emit '{"type":"system","subtype":"session_state_changed","state":"running"}'
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"pondering"}}}'
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hello"}}}'
  # Subagent frames (parent_tool_use_id set): tagged, never in the parent feed.
  emit '{"type":"stream_event","parent_tool_use_id":"sub-1","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"SUBAGENT"}}}'
  emit '{"type":"assistant","parent_tool_use_id":"sub-1","message":{"content":[{"type":"tool_use","id":"sub-tool","name":"Bash","input":{"command":"echo sub"}}]}}'
  emit '{"type":"user","parent_tool_use_id":"sub-1","message":{"content":[{"type":"tool_result","tool_use_id":"sub-tool","is_error":false}]}}'
  emit '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"Hello"},{"type":"tool_use","id":"tool-1","name":"Bash","input":{"command":"ls -la"}},{"type":"tool_use","id":"tool-2","name":"mcp__linear__search","input":{"q":"bug"}}]}}'
  emit '{"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":"tool-1","is_error":false},{"type":"tool_result","tool_use_id":"tool-2","is_error":true}]}}'
  # Informational rate-limit status: stays quiet.
  emit '{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}'
  emit '{"type":"result","subtype":"success","result":"done!","errors":[],"usage":{"input_tokens":10,"output_tokens":20},"session_id":"sess-1","total_cost_usd":0.01}'
  ;;

*scenario:wake*)
  # Eager-done + wake, the live-verified 2.1.228 background-subagent shape:
  # the parent turn settles with result #1 while the subagent still runs;
  # tagged subagent traffic continues; the CLI then wakes with a second init
  # (SAME session id) and settles the wake turn with result #2.
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Bash"],"cwd":"/tmp","session_id":"sess-wake"}'
  emit '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"toolu_agent","name":"Agent","input":{"description":"background task","run_in_background":true}}]}}'
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"LAUNCHED"}}}'
  emit '{"type":"result","subtype":"success","result":"LAUNCHED","errors":[],"usage":{"input_tokens":5,"output_tokens":5},"session_id":"sess-wake"}'
  # Background subagent interior, after the eager done.
  emit '{"type":"stream_event","parent_tool_use_id":"toolu_agent","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"sub working"}}}'
  emit '{"type":"assistant","parent_tool_use_id":"toolu_agent","message":{"content":[{"type":"tool_use","id":"sub-t1","name":"Bash","input":{"command":"sleep 1"}}]}}'
  emit '{"type":"user","parent_tool_use_id":"toolu_agent","message":{"content":[{"type":"tool_result","tool_use_id":"sub-t1","is_error":false}]}}'
  # The wake turn: re-init (deduped), untagged output, second result.
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Bash"],"cwd":"/tmp","session_id":"sess-wake"}'
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"subagent finished"}}}'
  emit '{"type":"result","subtype":"success","result":"wrapped up","errors":[],"usage":{"input_tokens":3,"output_tokens":3},"session_id":"sess-wake"}'
  ;;

*scenario:askuser*)
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Bash"],"cwd":"/tmp","session_id":"sess-ask"}'
  # A plain tool permission request: must be auto-allowed.
  emit '{"type":"control_request","request_id":"cr-0","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"}}}'
  read_reply resp0 || exit 1
  case "$resp0" in
  *'"request_id":"cr-0"'*'"behavior":"allow"'*) ;;
  *)
    emit '{"type":"result","subtype":"error_during_execution","errors":["bash tool was not allowed"],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-ask"}'
    exit 0
    ;;
  esac
  # AskUserQuestion: must be intercepted and answered via updatedInput.answers.
  emit '{"type":"control_request","request_id":"cr-1","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","input":{"questions":[{"header":"Choice","question":"Pick one","options":["A","B"],"multiSelect":false}]}}}'
  read_reply resp1 || exit 1
  case "$resp1" in
  *'"behavior":"allow"'*)
    case "$resp1" in
    *'"Pick one":"B"'*)
      emit '{"type":"result","subtype":"success","result":"answered","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-ask"}'
      ;;
    *)
      emit '{"type":"result","subtype":"error_during_execution","errors":["answers missing from updatedInput"],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-ask"}'
      ;;
    esac
    ;;
  *)
    emit '{"type":"result","subtype":"error_during_execution","errors":["AskUserQuestion was denied"],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-ask"}'
    ;;
  esac
  ;;

*scenario:mode-change*)
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Bash"],"cwd":"/tmp","session_id":"sess-mode"}'
  read -r change || exit 1
  case "$change" in
  *'"subtype":"set_permission_mode"'*'"mode":"default"'*) ;;
  *'"subtype":"set_permission_mode"'*'"mode":"plan"'*)
    emit '{"type":"result","subtype":"success","result":"planning","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-mode"}'
    exit 0
    ;;
  *)
    emit '{"type":"result","subtype":"error_during_execution","errors":["no mode change"],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-mode"}'
    exit 0
    ;;
  esac
  emit '{"type":"control_request","request_id":"cr-0","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"}}}'
  read_reply resp0 || exit 1
  case "$resp0" in
  *'"request_id":"cr-0"'*'"behavior":"allow"'*)
    emit '{"type":"result","subtype":"success","result":"allowed","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-mode"}'
    ;;
  *)
    emit '{"type":"result","subtype":"error_during_execution","errors":["bash tool was not allowed"],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-mode"}'
    ;;
  esac
  ;;

*scenario:plan-approval*)
  # The Allow must come after the CLI confirmed the mode change; "auto-refused" refuses auto like a plan without it, "auto-exits" quits without answering.
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-plan","permissionMode":"plan"}'
  emit '{"type":"control_request","request_id":"cr-plan","request":{"subtype":"can_use_tool","tool_name":"ExitPlanMode","input":{"plan":"# Plan"},"tool_use_id":"tu-plan"}}'
  confirmed=plan
  while read -r line; do
    case "$line" in
    *'"subtype":"set_permission_mode"'*)
      rid=$(request_id "$line")
      mode=$(printf '%s\n' "$line" | sed 's/.*"mode":"\([^"]*\)".*/\1/')
      case "$first:$mode" in
      *auto-exits*:auto) exit 0 ;;
      *auto-refused*:auto)
        emit "{\"type\":\"control_response\",\"response\":{\"subtype\":\"error\",\"request_id\":\"$rid\",\"error\":\"auto mode is unavailable\"}}"
        ;;
      *)
        confirmed=$mode
        emit "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$rid\",\"response\":{}}}"
        ;;
      esac
      ;;
    *'"request_id":"cr-plan"'*'"behavior":"allow"'*)
      text_delta "approved in $confirmed"
      emit '{"type":"result","subtype":"success","result":"approved","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-plan"}'
      ;;
    *'"request_id":"cr-plan"'*)
      emit '{"type":"result","subtype":"error_during_execution","errors":["plan was not approved"],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-plan"}'
      ;;
    esac
  done
  ;;

*scenario:silent-mode*)
  # A mode the session adopted silently must change prompts without a set_permission_mode line.
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Bash"],"cwd":"/tmp","session_id":"sess-silent"}'
  read -r line || exit 1
  case "$line" in
  *'"subtype":"set_permission_mode"'*)
    emit '{"type":"result","subtype":"error_during_execution","errors":["the mode was sent back"],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-silent"}'
    exit 0
    ;;
  esac
  emit '{"type":"control_request","request_id":"cr-0","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"}}}'
  read -r resp0 || exit 1
  case "$resp0" in
  *'"request_id":"cr-0"'*'"behavior":"allow"'*)
    emit '{"type":"result","subtype":"success","result":"allowed","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-silent"}'
    ;;
  *)
    emit '{"type":"result","subtype":"error_during_execution","errors":["bash tool was not allowed"],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-silent"}'
    ;;
  esac
  ;;

*scenario:record*)
  # Logs every user line to .sent-prompts and answers each one, so agent_chat tests can see what each message carried.
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-record"}'
  line=$first
  while :; do
    case "$line" in
    *'"type":"user"'*)
      printf '%s\n' "$line" >> .sent-prompts
      emit "$line"
      emit '{"type":"result","subtype":"success","result":"recorded","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-record"}'
      ;;
    esac
    read -r line || exit 0
  done
  ;;

*scenario:steer*)
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-steer"}'
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"first"}}}'
  # The queued steering user line, applied at "the step boundary" (here: now).
  read -r steer || exit 1
  case "$steer" in *'"priority":"now"'*) ;; *) exit 9 ;; esac
  # Old output continues until Claude actually consumes the input.
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"-still-first"}}}'
  emit "$steer"
  content=$(printf '%s\n' "$steer" | sed 's/.*"content":"\([^"]*\)".*/\1/')
  emit "{\"type\":\"stream_event\",\"parent_tool_use_id\":null,\"event\":{\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"steered:$content\"}}}"
  emit '{"type":"result","subtype":"success","result":"steered","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-steer"}'
  ;;

*scenario:images*)
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-img"}'
  [ "$(printf '%s' "$first" | grep -o '"type":"image"' | wc -l)" -eq 1 ] || exit 9
  for want in '"type":"base64"' '"media_type":"image/png"' '"data":"aGVsbG8="' '"type":"text"'; do
    case "$first" in *"$want"*) ;; *) exit 9 ;; esac
  done
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"first-image-ok"}}}'
  read -r steer || exit 1
  for want in '"type":"image"' '"media_type":"image/jpeg"' '"data":"d29ybGQ="' '"type":"text"'; do
    case "$steer" in *"$want"*) ;; *) exit 9 ;; esac
  done
  emit "$steer"
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"steer-image-ok"}}}'
  emit '{"type":"result","subtype":"success","result":"images","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-img"}'
  ;;

*scenario:superseded-steers*)
  # CLI 2.1.280 with rapid `now` steers: the second interrupts the turn the
  # first started before it is replayed; only the last steer is replayed.
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-sup"}'
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"story"}}}'
  read -r steer1 || exit 1
  read -r steer2 || exit 1
  emit '{"type":"result","subtype":"success","result":"story","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-sup"}'
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-sup"}'
  emit '{"type":"result","subtype":"error_during_execution","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-sup"}'
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-sup"}'
  emit "$steer2"
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"answered-both"}}}'
  emit '{"type":"result","subtype":"success","result":"answered-both","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-sup"}'
  ;;

*scenario:absorbed-steer*)
  # A steer whose replay never comes: the turn end must not be held forever.
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-abs"}'
  read -r steer || exit 1
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"absorbed"}}}'
  emit '{"type":"result","subtype":"success","result":"absorbed","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-abs"}'
  exec sleep 30
  ;;

*scenario:drop*)
  uuid=$(printf '%s\n' "$first" | sed 's/.*"uuid":"\([^"]*\)".*/\1/')
  resumed="fresh"
  for arg in "$@"; do
    case "$arg" in
      --resume-session-at=*) resumed="at ${arg#--resume-session-at=}" ;;
      --resume=*) [ "$resumed" = fresh ] && resumed="whole"; source=${arg#--resume=} ;;
      --fork-session) forked=true ;;
    esac
  done
  [ -n "$forked" ] && resumed="$resumed, forked from $source"
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-drop"}'
  emit "{\"type\":\"user\",\"uuid\":\"$uuid\",\"parent_tool_use_id\":null,\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"scenario:drop\"}]}}"
  emit "{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"$resumed\"}}}"
  emit '{"type":"result","subtype":"success","result":"done","usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-drop"}'
  ;;

*scenario:interrupt*)
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-int"}'
  # Wedge without reading stdin to force the SIGTERM escalation path.
  exec sleep 30
  ;;

*'"subtype":"initialize"'*)
  # Command discovery: the initialize control request arrives as the FIRST
  # stdin line (no user message ever follows).
  answer_initialize "$first"
  # Stay alive until the driver tears us down, like the real CLI would.
  exec sleep 30
  ;;

*scenario:control*|*'"type":"control_request"'*)
  # A run's turn or a one-off control call (the request comes right after initialize); a `hang` request is never answered.
  answer_control() {
    rid=$(request_id "$1")
    case "$1" in
    *'"subtype":"get_plan"'*)
      emit "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$rid\",\"response\":{\"exists\":true,\"content\":\"the plan\",\"path\":\"plan.md\"}}}"
      ;;
    *'"subtype":"set_permission_mode"'*)
      emit "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$rid\",\"response\":{}}}"
      ;;
    *'"subtype":"mcp_status"'*)
      emit "{\"type\":\"control_response\",\"response\":{\"subtype\":\"error\",\"request_id\":\"$rid\",\"error\":\"MCP servers are unavailable\"}}"
      ;;
    *'"subtype":"rewind_files"'*) answer_rewind "$1" "idle" ;;
    esac
  }
  case "$first" in
  *'"type":"control_request"'*) answer_control "$first" ;;
  *)
    emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-control"}'
    emit '{"type":"result","subtype":"success","result":"ready","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-control"}'
    ;;
  esac
  while read -r line; do
    case "$line" in
    *'"type":"control_request"'*) answer_control "$line" ;;
    esac
  done
  ;;

*scenario:stop*)
  # One process, many turns: an interrupt ends the running turn, a user line starts the next.
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-stop"}'
  text_delta "turn 1 in $$"
  turn=1
  running=true
  case "$first" in *scenario:stop-idle*)
    emit '{"type":"result","subtype":"success","result":"first","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-stop"}'
    running=false
    ;;
  *scenario:stop-asking*)
    emit '{"type":"control_request","request_id":"ask-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"}}}'
    ;;
  esac
  while read -r line; do
    case "$line" in
    *'"type":"control_response"'*) text_delta "late answer" ;;
    *'"subtype":"rewind_files"'*)
      if [ "$running" = true ]; then answer_rewind "$line" running; else answer_rewind "$line" idle; fi
      ;;
    *'"subtype":"interrupt"'*)
      emit "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$(request_id "$line")\"}}"
      if [ "$running" = true ]; then
        emit '{"type":"result","subtype":"error_during_execution","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-stop"}'
      else
        emit '{"type":"result","subtype":"error_during_execution","errors":["interrupted while idle"],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-stop"}'
      fi
      running=false
      ;;
    *'"type":"user"'*)
      turn=$((turn + 1))
      emit "$line"
      text_delta "turn $turn in $$: $(text_of "$line")"
      emit '{"type":"result","subtype":"success","result":"next","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-stop"}'
      ;;
    esac
  done
  ;;

*scenario:models*)
  model=default
  while [ "$#" -gt 0 ]; do
    case "$1" in --model) shift; model=$1 ;; esac
    shift
  done
  switches=0
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-models"}'
  text_delta "model=$model switches=$switches"
  emit '{"type":"result","subtype":"success","result":"first","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-models"}'
  while read -r line; do
    case "$line" in
    *'"subtype":"set_model"'*)
      wanted=$(printf '%s\n' "$line" | sed 's/.*"model":"\([^"]*\)".*/\1/')
      case "$wanted" in
      *unknown*)
        emit "{\"type\":\"control_response\",\"response\":{\"subtype\":\"error\",\"request_id\":\"$(request_id "$line")\",\"error\":\"no such model\"}}"
        ;;
      *)
        model=$wanted
        switches=$((switches + 1))
        emit "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$(request_id "$line")\"}}"
        ;;
      esac
      ;;
    *'"type":"user"'*)
      emit "$line"
      text_delta "model=$model switches=$switches"
      emit '{"type":"result","subtype":"success","result":"next","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-models"}'
      ;;
    esac
  done
  ;;

*scenario:hook*)
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Read"],"cwd":"/tmp","session_id":"sess-hook"}'
  case "$init_line" in
  *'"matcher":"Edit|Write|MultiEdit|Read"'*) ;;
  *)
    emit '{"type":"result","subtype":"error_during_execution","errors":["initialize had no save hook"],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-hook"}'
    exit 0
    ;;
  esac
  case "$init_line" in
  *'"hookCallbackIds":["wu-presave"]'*) ;;
  *)
    emit '{"type":"result","subtype":"error_during_execution","errors":["initialize had no save hook"],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-hook"}'
    exit 0
    ;;
  esac
  hook_path=/x/a.rs
  if [ -f .hook-path ]; then hook_path=$(cat .hook-path); fi
  # Shape mirrors 2.1.331's hook_callback for a PreToolUse hook.
  emit "{\"type\":\"control_request\",\"request_id\":\"hk-1\",\"request\":{\"subtype\":\"hook_callback\",\"callback_id\":\"wu-presave\",\"input\":{\"session_id\":\"sess-hook\",\"hook_event_name\":\"PreToolUse\",\"tool_name\":\"Read\",\"tool_input\":{\"file_path\":\"$hook_path\"}},\"tool_use_id\":\"tu-1\"}}"
  read_reply hook_reply || exit 1
  case "$hook_reply" in
  *'"request_id":"hk-1"'*'"continue":true'* | *'"continue":true'*'"request_id":"hk-1"'*)
    emit '{"type":"result","subtype":"success","result":"read it","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-hook"}'
    ;;
  *)
    emit '{"type":"result","subtype":"error_during_execution","errors":["the save hook was not answered with continue"],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-hook"}'
    ;;
  esac
  ;;

*scenario:mcp*)
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-mcp"}'
  case "$init_line" in
  *'"sdkMcpServers":["ide"]'*) ;;
  *)
    emit '{"type":"result","subtype":"error_during_execution","errors":["initialize had no ide server"],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-mcp"}'
    exit 0
    ;;
  esac
  # Shape mirrors 2.1.332's mcp_message for an in-process server.
  emit '{"type":"control_request","request_id":"mcp-1","request":{"subtype":"mcp_message","server_name":"ide","message":{"method":"tools/list","jsonrpc":"2.0","id":1}}}'
  read_reply mcp_reply || exit 1
  printf '%s\n' "$mcp_reply" > .mcp-reply
  emit '{"type":"result","subtype":"success","result":"listed","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-mcp"}'
  ;;

*scenario:commands*)
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-commands"}'
  emit '{"type":"result","subtype":"success","result":"done","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-commands"}'
  ;;

*scenario:error*)
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-err"}'
  # Terse assistant-level error code with no content.
  emit '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[]},"error":"rate_limit"}'
  # Hard-rejected claude.ai usage window.
  emit '{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","rateLimitType":"five_hour"}}'
  # Result error with an EMPTY errors array: needs fallback wording.
  emit '{"type":"result","subtype":"error_max_turns","errors":[],"usage":{"input_tokens":1,"output_tokens":2},"session_id":"sess-err"}'
  ;;

*)
  emit '{"type":"result","subtype":"error_during_execution","errors":["unknown scenario"],"usage":{"input_tokens":0,"output_tokens":0},"session_id":"sess-x"}'
  ;;
esac
