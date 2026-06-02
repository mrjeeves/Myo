//! The `shell` tool — run a command on the user's machine, streaming its output.
//!
//! Ported from MyOwnLLM's `agent_io::run_shell_inner` (the parallel
//! stdout/stderr drain, output cap, kill-on-drop, lossy decode, timeout), with
//! one upgrade that matters for a real agent: instead of capturing to EOF and
//! reporting once, it **streams each chunk live** as an `ActivityProgress` while
//! the command runs. A long-running build, install, or test run shows its output
//! as it happens — and still returns the full (capped) capture for the model.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

use crate::event::{MyoEvent, TurnId};

use super::{Category, Tool, ToolCtx, ToolResult};

/// Hard cap on captured output bytes per stream — a model that runs
/// `cat /dev/urandom` shouldn't OOM the app. Past this we keep draining (so the
/// child doesn't block on a full pipe) but stop buffering, and flag the result.
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
/// Default per-command budget. Generous on purpose: the point of the tool is to
/// let the agent launch real, long-running work and watch it, not to clip it.
const DEFAULT_TIMEOUT_MS: u64 = 10 * 60 * 1000;
/// Ceiling on a per-call timeout, so a confused model can't park the loop for a
/// day by asking for a 24-hour budget.
const MAX_TIMEOUT_MS: u64 = 60 * 60 * 1000;

pub struct ShellTool;

#[async_trait::async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn category(&self) -> Category {
        Category::Code
    }

    fn headline(&self, args: &Value) -> Option<String> {
        args.get("command")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn schema(&self) -> Value {
        let shell = if cfg!(windows) { "cmd /C" } else { "sh -c" };
        json!({
            "type": "function",
            "function": {
                "name": "shell",
                "description": format!(
                    "Run a shell command on the user's machine (via {shell}) and return its \
                     stdout, stderr, and exit code. Use it to list files, run programs, use git, \
                     install packages, or carry out any system task. Output streams live, so \
                     long-running commands are fine."
                ),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "The command line to run." },
                        "cwd": { "type": "string", "description": "Optional working directory to run in." },
                        "timeout_ms": {
                            "type": "integer",
                            "description": "Optional wall-clock timeout in milliseconds (default 600000, max 3600000)."
                        }
                    },
                    "required": ["command"]
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult> {
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("shell requires a 'command' string"))?
            .to_string();
        if command.trim().is_empty() {
            return Err(anyhow!("command must be non-empty"));
        }
        let cwd = args
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let timeout = Duration::from_millis(
            args.get("timeout_ms")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_TIMEOUT_MS)
                .clamp(1, MAX_TIMEOUT_MS),
        );

        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/C", &command]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", &command]);
            c
        };
        if let Some(dir) = cwd.as_deref() {
            cmd.current_dir(dir);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = cmd.spawn().context("spawn shell")?;
        let stdout_h = child.stdout.take().context("take stdout")?;
        let stderr_h = child.stderr.take().context("take stderr")?;

        // Drain both streams in parallel — a child that floods stderr while we
        // only read stdout (or vice versa) must not deadlock on the pipe buffer —
        // and stream each chunk live as it arrives.
        let tool = self.name().to_string();
        let stdout_task = tokio::spawn(drain_stream(
            stdout_h,
            tool.clone(),
            ctx.turn,
            ctx.round,
            ctx.events.clone(),
        ));
        let stderr_task = tokio::spawn(drain_stream(
            stderr_h,
            tool.clone(),
            ctx.turn,
            ctx.round,
            ctx.events.clone(),
        ));

        let timed_out;
        let exit_code;
        match tokio::time::timeout(timeout, child.wait()).await {
            Ok(Ok(status)) => {
                timed_out = false;
                exit_code = status.code().map(|c| c as i64);
            }
            Ok(Err(e)) => return Err(anyhow!("wait child: {e}")),
            Err(_) => {
                timed_out = true;
                let _ = child.start_kill();
                let _ = child.wait().await;
                exit_code = None;
            }
        }

        let (stdout, stdout_trunc) = stdout_task
            .await
            .map_err(|e| anyhow!("stdout join: {e}"))??;
        let (stderr, stderr_trunc) = stderr_task
            .await
            .map_err(|e| anyhow!("stderr join: {e}"))??;

        let text = render_result(
            exit_code,
            timed_out,
            &stdout,
            stdout_trunc,
            &stderr,
            stderr_trunc,
        );
        Ok(ToolResult { text, exit_code })
    }
}

/// Read a child stream to EOF, streaming each decoded chunk as an
/// `ActivityProgress` and accumulating up to [`MAX_OUTPUT_BYTES`] for the
/// returned capture. Bytes past the cap are streamed but not buffered.
async fn drain_stream<R>(
    mut reader: R,
    tool: String,
    turn: TurnId,
    round: u64,
    events: UnboundedSender<MyoEvent>,
) -> Result<(String, bool)>
where
    R: AsyncRead + Unpin,
{
    let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut tmp = [0u8; 8 * 1024];
    let mut truncated = false;
    loop {
        let n = reader.read(&mut tmp).await.context("read stream")?;
        if n == 0 {
            break;
        }
        // Stream the chunk live (best-effort; a cancelled turn closes the channel).
        let chunk = String::from_utf8_lossy(&tmp[..n]).into_owned();
        let _ = events.send(MyoEvent::ActivityProgress {
            turn,
            tool: tool.clone(),
            progress: Some(chunk),
            round: Some(round),
        });
        // Accumulate up to the cap for the model-facing capture.
        let remaining = MAX_OUTPUT_BYTES.saturating_sub(buf.len());
        if remaining == 0 {
            truncated = true;
            continue;
        }
        let take = n.min(remaining);
        buf.extend_from_slice(&tmp[..take]);
        if take < n {
            truncated = true;
        }
    }
    Ok((String::from_utf8_lossy(&buf).into_owned(), truncated))
}

/// Shape the captured run into the text the model reads back: a status line, then
/// the stdout/stderr sections (omitting empty ones), with truncation/timeout
/// notes so the model knows when output was clipped or the command was killed.
fn render_result(
    exit_code: Option<i64>,
    timed_out: bool,
    stdout: &str,
    stdout_trunc: bool,
    stderr: &str,
    stderr_trunc: bool,
) -> String {
    let mut out = String::new();
    if timed_out {
        out.push_str("Command timed out and was killed before it exited.\n");
    } else {
        match exit_code {
            Some(c) => out.push_str(&format!("Exit code: {c}\n")),
            None => out.push_str("Command exited without a status code.\n"),
        }
    }
    if !stdout.trim().is_empty() {
        out.push_str("\n[stdout]\n");
        out.push_str(stdout);
        if stdout_trunc {
            out.push_str("\n…(stdout truncated)");
        }
        out.push('\n');
    }
    if !stderr.trim().is_empty() {
        out.push_str("\n[stderr]\n");
        out.push_str(stderr);
        if stderr_trunc {
            out.push_str("\n…(stderr truncated)");
        }
        out.push('\n');
    }
    if stdout.trim().is_empty() && stderr.trim().is_empty() && !timed_out {
        out.push_str("\n(no output)\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn ctx() -> (ToolCtx, mpsc::UnboundedReceiver<MyoEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let web = std::sync::Arc::new(
            crate::tools::web::WebSearch::new(crate::config::WebSearchConfig::Ddg).unwrap(),
        );
        (
            ToolCtx {
                turn: 1,
                round: 0,
                web,
                events: tx,
            },
            rx,
        )
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_and_streams_then_captures() {
        let (c, mut rx) = ctx();
        let out = ShellTool
            .execute(json!({ "command": "echo hello" }), &c)
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(out.text.contains("hello"));
        assert!(out.text.contains("Exit code: 0"));
        // The chunk was also streamed live as progress.
        drop(c); // close the sender so recv drains then ends
        let mut streamed = String::new();
        while let Some(ev) = rx.recv().await {
            if let MyoEvent::ActivityProgress {
                progress: Some(p), ..
            } = ev
            {
                streamed.push_str(&p);
            }
        }
        assert!(streamed.contains("hello"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn separates_stderr_and_exit() {
        let (c, _rx) = ctx();
        let out = ShellTool
            .execute(json!({ "command": "echo out; echo err 1>&2; exit 3" }), &c)
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(3));
        assert!(out.text.contains("out"));
        assert!(out.text.contains("err"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn times_out() {
        let (c, _rx) = ctx();
        let out = ShellTool
            .execute(json!({ "command": "sleep 5", "timeout_ms": 200 }), &c)
            .await
            .unwrap();
        assert!(out.exit_code.is_none());
        assert!(out.text.contains("timed out"));
    }

    #[tokio::test]
    async fn rejects_empty_command() {
        let (c, _rx) = ctx();
        assert!(ShellTool
            .execute(json!({ "command": "   " }), &c)
            .await
            .is_err());
        assert!(ShellTool.execute(json!({}), &c).await.is_err());
    }
}
