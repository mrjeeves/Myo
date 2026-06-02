//! Myo's native tool kit — the things the agent can actually *do*.
//!
//! The native brain ([`crate::llm`]) can now answer a turn with `tool_calls`
//! instead of (or before) prose; this module is what runs them. A [`Tool`] pairs
//! an OpenAI function schema (so the model knows the shape to fill in) with an
//! async `execute` that does the work and returns text the model reads back on
//! the next round. The [`converse`](crate::converse) loop drives the cycle:
//! offer the enabled tools, run whatever the model calls, feed the results back,
//! repeat until it answers.
//!
//! Design notes:
//!   * **Capability-gated.** [`registry`] only ever builds the tools whose
//!     category toggle ([`Capabilities`]) is on, so a disabled category is never
//!     even advertised — and the loop's [`find`] backstop means a model that
//!     hallucinates a disabled name gets a clean "disabled" result, not an
//!     execution.
//!   * **Long-running + streaming.** Tools stream intermittent output live via
//!     [`ToolCtx::progress`] (an `ActivityProgress` per chunk) while they run,
//!     rather than only reporting once at the end. The loop runs a turn's tool
//!     calls concurrently, so several long tools make progress at once.
//!   * **Extensible.** Adding a tool (a `research` pipeline, an MCP bridge) is a
//!     new [`Tool`] impl plus a line in [`registry`]; nothing else moves.
//!
//! Safety stance (this revision): shell and file tools run as the installed user
//! with no sandbox — the same posture as MyOwnLLM's `agent_io`. They're gated
//! behind the Code / Files toggles (both default **off**), and every call
//! renders as an activity pill so actions are visible. Per-action approval
//! (tier-b) is future work.

use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::capabilities::Capabilities;
use crate::event::{MyoEvent, TurnId};

pub mod files;
pub mod shell;
pub mod web;

pub use web::{Hit, WebSearch};

/// Which capability toggle a tool belongs to. The single source of truth for
/// whether a tool is offered at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Web,
    Files,
    Code,
}

impl Category {
    /// Is this category's toggle on in `caps`?
    pub fn enabled(self, caps: Capabilities) -> bool {
        match self {
            Category::Web => caps.web,
            Category::Files => caps.files,
            Category::Code => caps.code,
        }
    }
}

/// What a tool hands back to the model. `text` is what the model reads; an
/// optional `exit_code` rides along to the UI's activity pill.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub text: String,
    pub exit_code: Option<i64>,
}

impl ToolResult {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            exit_code: None,
        }
    }
}

/// The ambient context a tool executes in: the turn/round it belongs to, the
/// shared web-search client, and the event channel it streams progress on.
///
/// `events` is an mpsc sender the [`converse`](crate::converse) loop drains onto
/// the real `emit` while the tool runs — so a long tool's intermittent output
/// reaches the UI live, even with several tools running at once.
pub struct ToolCtx {
    pub turn: TurnId,
    pub round: u64,
    pub web: Arc<WebSearch>,
    pub events: UnboundedSender<MyoEvent>,
}

impl ToolCtx {
    /// Stream a chunk of intermittent output for `tool` (an `ActivityProgress`).
    /// Best-effort: a closed receiver (turn cancelled) just drops the chunk.
    pub fn progress(&self, tool: &str, chunk: impl Into<String>) {
        let _ = self.events.send(MyoEvent::ActivityProgress {
            turn: self.turn,
            tool: tool.to_string(),
            progress: Some(chunk.into()),
            round: Some(self.round),
        });
    }
}

/// One tool in the kit. Object-safe (`Arc<dyn Tool>`) so a heterogeneous set
/// lives in one registry and the loop dispatches by name.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// The function name the model calls (matches the schema).
    fn name(&self) -> &str;
    /// The OpenAI tool schema advertised to the model.
    fn schema(&self) -> Value;
    /// Which capability toggle gates this tool.
    fn category(&self) -> Category;
    /// A short headline for the activity pill (e.g. the shell command, the search
    /// query, the file path), derived from the call's arguments. `None` ⇒ no
    /// headline.
    fn headline(&self, _args: &Value) -> Option<String> {
        None
    }
    /// Run the call and return text for the model. Errors become a tool result
    /// the model can read and recover from, so the loop never dies on a bad call.
    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult>;
}

/// Build the tool kit for the given capabilities — only the tools whose category
/// is toggled on. The order here is the order they're advertised to the model.
pub fn registry(caps: Capabilities) -> Vec<Arc<dyn Tool>> {
    let all: Vec<Arc<dyn Tool>> = vec![
        Arc::new(shell::ShellTool),
        Arc::new(files::ReadFileTool),
        Arc::new(files::WriteFileTool),
        Arc::new(web::WebSearchTool),
    ];
    all.into_iter()
        .filter(|t| t.category().enabled(caps))
        .collect()
}

/// The OpenAI `tools` array for a built registry.
pub fn tool_schemas(tools: &[Arc<dyn Tool>]) -> Vec<Value> {
    tools.iter().map(|t| t.schema()).collect()
}

/// Find a tool by name in a built registry (the runtime gate backstop). Returns
/// `None` for a disabled/unknown name, which the loop turns into a "disabled"
/// tool result rather than an execution.
pub fn find(tools: &[Arc<dyn Tool>], name: &str) -> Option<Arc<dyn Tool>> {
    tools.iter().find(|t| t.name() == name).cloned()
}

/// The system addendum appended to a turn when any tool is enabled. It tells the
/// model the tools are real and that tool *arguments* (commands, paths, queries)
/// are exempt from the voice-only writing rules the persona imposes on prose.
pub const TOOL_PREAMBLE: &str = "\
You have a set of real tools available, described in the tools list, that act on \
the user's own machine: you can run shell commands, read and write files, and \
search the web. Use them whenever doing so actually helps — to check something, \
fetch a fact, or carry out a task — rather than guessing or claiming you can't. \
The voice-only writing rules apply to what you say out loud, not to tool \
arguments: write shell commands, file paths, and search queries normally and \
precisely. As you work, narrate what you're doing in a natural, spoken way, and \
once the tools have done their job, give the user a short, plain-spoken answer.";

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(web: bool, files: bool, code: bool) -> Capabilities {
        Capabilities {
            web,
            files,
            code,
            reach_out: false,
        }
    }

    #[test]
    fn registry_offers_only_enabled_categories() {
        // Default posture: web on, files/code off → only web_search.
        let r = registry(caps(true, false, false));
        let names: Vec<&str> = r.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["web_search"]);

        // Code on → shell shows up; files still gated out.
        let r = registry(caps(false, false, true));
        let names: Vec<&str> = r.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["shell"]);

        // Everything on → the whole kit.
        let r = registry(caps(true, true, true));
        let names: Vec<&str> = r.iter().map(|t| t.name()).collect();
        assert_eq!(
            names,
            vec!["shell", "read_file", "write_file", "web_search"]
        );
    }

    #[test]
    fn find_is_a_gate_backstop() {
        let r = registry(caps(false, false, true)); // only shell
        assert!(find(&r, "shell").is_some());
        // A disabled tool isn't in the registry, so find says no → loop refuses it.
        assert!(find(&r, "read_file").is_none());
        assert!(find(&r, "nonexistent").is_none());
    }

    #[test]
    fn schemas_are_well_formed_openai_functions() {
        let r = registry(caps(true, true, true));
        let schemas = tool_schemas(&r);
        assert_eq!(schemas.len(), 4);
        for s in &schemas {
            assert_eq!(s["type"], "function");
            assert!(s["function"]["name"].is_string());
            assert_eq!(s["function"]["parameters"]["type"], "object");
        }
    }
}
