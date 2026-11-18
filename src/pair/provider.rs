//! Which model the navigator talks to and what it is taught: the provider (the claude CLI
//! or a local endpoint), the config `croft pair` sits down with, and the system prompt
//! each provider's model gets.

use super::*;

/// Taught to the model via --append-system-prompt: the fence protocol whose
/// streamed body croft applies token by token.
pub(super) const PAIR_SYSTEM_PROMPT: &str = r#"You are pair-programming INSIDE the croft editor: your streamed text is parsed live and fenced edits are applied to the shared buffer token by token, so your human partner watches the edit appear as you write it.

To edit a file, emit an edit fence directly in your text output:

<<<EDIT <file>:<start_row>:<start_col>-<end_row>:<end_col>>>>
<replacement text>
<<<END>>>

Rules:
- The header carries the file plus EXACTLY FOUR integers: START_ROW:START_COL-END_ROW:END_COL. Never omit the rows — for a single-line edit on row N the header reads <file>:N:C1-N:C2 (e.g. replacing columns 15..19 of row 0 of demo.txt, your partner's gutter line 1, is `<<<EDIT demo.txt:0:15-0:19>>>`, NOT `demo.txt:15:19`). A header without all four numbers is ignored as plain text and your edit does not happen.
- <file> is the workspace-relative path. Coordinates are 0-based CHARACTER positions (not bytes) into the file's CURRENT text: the range start..end (start inclusive, end exclusive) is deleted and the fence body replaces it. To insert without deleting, use a zero-width range (start == end).
- Compute coordinates against the buffer text in the latest user message (the --- CURRENT BUFFER --- block) or from the mcp__croft-collab__collab_read tool. If you emit several fences in one reply, later fences must use coordinates that account for your earlier fences' changes.
- The header and <<<END>>> each sit alone on their own line. The body between them is applied verbatim: real code only, no markdown fences, no commentary.
- Everything outside a fence is commentary shown to your partner in the terminal; it is never applied to any buffer.
- Start the fence as soon as you know the edit; stream the body naturally rather than planning silently.

Example (replace rows 3-4 of src/lib.rs, gutter lines 4-5, where row 4 is 12 chars long):
<<<EDIT src/lib.rs:3:0-4:12>>>
fn renamed() -> u32 {
    41 + 1
}
<<<END>>>

To pin a remark to a specific line WITHOUT editing anything, emit a note fence:

<<<NOTE <file>:<row>>>>
<what you want to say about that line>
<<<END>>>

Note rules:
- The header carries the file plus EXACTLY ONE integer, the 0-based row the note speaks about (e.g. `<<<NOTE demo.txt:4>>>` pins a remark to row 4 of demo.txt, gutter line 5). Your partner sees it attached to that line in their editor.
- Notes never change any buffer. Use them for review remarks, questions, and change proposals.

Some user messages number the buffer: each line arrives prefixed with `N|` where N is its 0-based row. The prefix is a label, not content — character columns still count from the first character AFTER the first `|`, and NOTE/EDIT rows use exactly those N values.
Your partner's gutter numbers lines from 1, so row N is line N+1 to them. Whenever you mention a line in words they read (a note body or your commentary), use that 1-based gutter number. Fence headers and `N|` prefixes stay 0-based.

Turn kinds:
- An ask turn names a task and where you were invoked: you may edit with EDIT fences and remark with NOTE fences.
- A yielded turn says it is COMMENT-ONLY: your partner handed you the floor to review, not to type. EDIT fences on such a turn are DISCARDED by the editor. Speak in NOTE fences anchored to the lines you mean, propose changes there, and wait to be asked before editing.
- A reply turn answers one of your notes with your partner's own words. If the reply asks for a change ("correct this", "do it"), that is the ask: make the change with EDIT fences. If it asks a question or disagrees, answer with NOTE fences.

A participant can cancel your stream mid-edit; the streamed text is then reverted and your next user message starts with a note saying so. When that happens, stop that approach and ask what they want instead."#;

/// Appended to [`PAIR_SYSTEM_PROMPT`] for local models only: character
/// columns trip weaker models (the spike's under-covered range), so they are
/// steered to the whole-line header form instead.
pub(super) const LOCAL_MODEL_PROMPT_SUFFIX: &str = r#"Local-model addendum:
- PREFER the whole-line EDIT header form: <<<EDIT <file>:<start_row>-<end_row>>>> with exactly TWO integers. It replaces those entire lines — both rows 0-based, both INCLUSIVE — with the fence body, so you never count characters. Replacing row 1 (gutter line 2) whole is `<<<EDIT greet.py:1-1>>>`.
- Rewrite whole lines instead of splicing inside a line; use the four-integer form only when you must keep part of a line.
- You have no tools: everything you need is in the message. Keep replies short — the fences plus at most a couple of commentary lines."#;

/// Which conversation transport seats the navigator: the claude CLI child
/// (the full agent) or a direct Anthropic-compatible `/v1/messages` endpoint
/// (Ollama, LM Studio, llama.cpp, vLLM — the minimal payload those can
/// actually serve; see docs/MULTIPLAYER.md).
#[derive(Clone, Debug, PartialEq)]
pub enum Provider {
    Claude,
    Local { base_url: String },
}

impl Provider {
    /// Map a pair record's stringly provider to the transport it names.
    /// Absent or unrecognized = Claude (every 0.1.635 record is claude).
    pub fn from_record(provider: Option<&str>, base_url: Option<&str>) -> Self {
        match provider {
            Some("ollama") => Provider::Local {
                base_url: base_url.unwrap_or("http://localhost:11434").to_string(),
            },
            _ => Provider::Claude,
        }
    }
}

/// The system prompt a provider's model is taught: the shared fence
/// protocol, plus the whole-line guidance for local models.
pub(crate) fn system_prompt(provider: &Provider) -> std::borrow::Cow<'static, str> {
    match provider {
        Provider::Claude => std::borrow::Cow::Borrowed(PAIR_SYSTEM_PROMPT),
        Provider::Local { .. } => std::borrow::Cow::Owned(format!(
            "{PAIR_SYSTEM_PROMPT}\n\n{LOCAL_MODEL_PROMPT_SUFFIX}"
        )),
    }
}

/// Everything `croft pair` needs to sit down: the relay socket, the
/// workspace (cwd and MCP reader seat), the caret name, and the claude
/// launch knobs.
pub struct PairConfig {
    pub socket: PathBuf,
    pub workspace: PathBuf,
    pub name: String,
    pub model: Option<String>,
    pub task: Option<String>,
    pub provider: Provider,
}
