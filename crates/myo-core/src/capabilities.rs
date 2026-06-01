//! The four tier-a capability toggles ⇄ Odysseus's existing permission knobs.
//!
//! Myo doesn't fork Odysseus to add per-action consent (that's tier-b, later).
//! For v1 it gets *coarse* control for free by driving knobs Odysseus already
//! has. A toggle composes onto two of them:
//!
//!   * **per-turn flags** — `allow_web_search` (Web) and `allow_bash` (Code)
//!     ride on every `chat_stream` request.
//!   * **persistent `disabled_tools`** — every tool in an *off* category is
//!     written to Odysseus's settings allowlist, so the agent can't call it.
//!
//! Myo connects over loopback as admin, so Odysseus's role gate — its normal
//! backstop for sensitive/infra tools — never fires for us. That means Myo must
//! disable the infra/vault/management set *explicitly* ([`ALWAYS_DISABLED`]);
//! it maps to no v1 category and is never exposed.
//!
//! Tool-name strings are reconciled against `docs/odysseus-integration.md` §11
//! and Odysseus's tool registry (`src/tool_schemas.py` / `tool_security.py`).

use serde::{Deserialize, Serialize};

/// The four categories the user (or the agent, via `ui_control toggle`) flips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Capabilities {
    /// Read the web — `web_search` + research.
    pub web: bool,
    /// Read and write files and documents.
    pub files: bool,
    /// Run code — `bash` / `python`.
    pub code: bool,
    /// Reach out to people — email, calendar, contacts.
    pub reach_out: bool,
}

impl Default for Capabilities {
    /// PLAN default posture: **Web on, everything else off.** The agent can look
    /// things up out of the box; touching files, running code, or contacting
    /// anyone is opt-in.
    fn default() -> Self {
        Self {
            web: true,
            files: false,
            code: false,
            reach_out: false,
        }
    }
}

/// Web — search and the research pipeline. (`use_research`/`use_web` are
/// per-turn *flags*, not tools; browser surfaces come via MCP, and there is no
/// `builtin_browser` tool to name here.)
const WEB_TOOLS: &[&str] = &["web_search", "trigger_research", "manage_research"];

/// Files — direct file IO plus the document workspace tools.
const FILES_TOOLS: &[&str] = &[
    "read_file",
    "write_file",
    "create_document",
    "edit_document",
    "update_document",
    "suggest_document",
    "manage_documents",
];

/// Code — the two execution tools (also gated per-turn by `allow_bash`).
const CODE_TOOLS: &[&str] = &["bash", "python"];

/// Reach-out — anything that contacts a human or touches their inbox/calendar.
/// Names verified against `src/tool_schemas.py` / `TOOL_TAGS`.
const REACHOUT_TOOLS: &[&str] = &[
    "send_email",
    "reply_to_email",
    "list_emails",
    "read_email",
    "list_email_accounts",
    "bulk_email",
    "archive_email",
    "delete_email",
    "mark_email_read",
    "manage_calendar",
    "resolve_contact",
    "manage_contact",
];

/// Infra / vault / model-serving / management — no v1 category. Disabled
/// unconditionally: Myo runs as admin, so Odysseus's role gate (the usual
/// backstop for these — `tool_security.NON_ADMIN_BLOCKED_TOOLS`) is open to us,
/// and we must close it ourselves. (Memory/skills/tasks are deliberately *not*
/// here — they're part of the continuous-presence companion.)
const ALWAYS_DISABLED: &[&str] = &[
    "api_call",
    "app_api",
    "vault_search",
    "vault_get",
    "vault_unlock",
    "manage_endpoints",
    "manage_mcp",
    "manage_webhooks",
    "manage_tokens",
    "manage_settings",
    "download_model",
    "serve_model",
    "stop_served_model",
    "cancel_download",
    "adopt_served_model",
];

impl Capabilities {
    /// The per-turn `allow_web_search` form flag for `chat_stream`.
    pub fn allow_web_search(&self) -> bool {
        self.web
    }

    /// The per-turn `allow_bash` form flag for `chat_stream`.
    pub fn allow_bash(&self) -> bool {
        self.code
    }

    /// The persistent `disabled_tools` allowlist to write to Odysseus: every
    /// tool of an *off* category, plus the always-off infra/vault set. Sorted
    /// and de-duplicated so the result is stable (handy for change-detection
    /// and for tests).
    pub fn disabled_tools(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if !self.web {
            push_all(&mut out, WEB_TOOLS);
        }
        if !self.files {
            push_all(&mut out, FILES_TOOLS);
        }
        if !self.code {
            push_all(&mut out, CODE_TOOLS);
        }
        if !self.reach_out {
            push_all(&mut out, REACHOUT_TOOLS);
        }
        push_all(&mut out, ALWAYS_DISABLED);
        out.sort_unstable();
        out.dedup();
        out
    }
}

fn push_all(out: &mut Vec<String>, names: &[&str]) {
    out.extend(names.iter().map(|s| s.to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_web_only() {
        let c = Capabilities::default();
        assert!(c.web && !c.files && !c.code && !c.reach_out);
        assert!(c.allow_web_search());
        assert!(!c.allow_bash());
    }

    #[test]
    fn off_categories_are_disabled() {
        let c = Capabilities::default(); // web on, rest off
        let disabled = c.disabled_tools();
        // Web is on → its tools are NOT disabled.
        assert!(!disabled.iter().any(|t| t == "web_search"));
        // Files/Code/Reach-out are off → representative tools disabled.
        assert!(disabled.iter().any(|t| t == "read_file"));
        assert!(disabled.iter().any(|t| t == "bash"));
        assert!(disabled.iter().any(|t| t == "send_email"));
    }

    #[test]
    fn everything_on_still_disables_infra() {
        let c = Capabilities {
            web: true,
            files: true,
            code: true,
            reach_out: true,
        };
        let disabled = c.disabled_tools();
        // No category tool is disabled…
        assert!(!disabled.iter().any(|t| t == "bash"));
        assert!(!disabled.iter().any(|t| t == "read_file"));
        // …but the infra/vault set always is.
        assert!(disabled.iter().any(|t| t == "vault_unlock"));
        assert!(disabled.iter().any(|t| t == "api_call"));
    }

    #[test]
    fn allow_flags_track_toggles() {
        let c = Capabilities {
            web: false,
            files: false,
            code: true,
            reach_out: false,
        };
        assert!(!c.allow_web_search());
        assert!(c.allow_bash());
        let disabled = c.disabled_tools();
        // Code on → bash/python not disabled; web off → web_search disabled.
        assert!(!disabled.iter().any(|t| t == "bash"));
        assert!(disabled.iter().any(|t| t == "web_search"));
    }

    #[test]
    fn disabled_list_is_sorted_and_deduped() {
        let disabled = Capabilities::default().disabled_tools();
        let mut sorted = disabled.clone();
        sorted.sort();
        assert_eq!(disabled, sorted, "must be sorted");
        sorted.dedup();
        assert_eq!(disabled.len(), sorted.len(), "must have no duplicates");
    }
}
