//! The `read_file` / `write_file` tools — direct, capped file I/O.
//!
//! Ported from MyOwnLLM's `agent_io::{read_file_inner, write_file_inner}`: read
//! caps at a byte budget so a huge log can't blow the context, write creates
//! parent directories by default and supports append. Both are fast enough that
//! they report once (no streaming) — the loop's start/output pills are plenty.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use super::{Category, Tool, ToolCtx, ToolResult};

/// Default read budget when the caller doesn't ask for one.
const DEFAULT_READ_BYTES: u64 = 1024 * 1024;
/// Ceiling on a single read, regardless of the requested `max_bytes`.
const MAX_READ_BYTES: u64 = 16 * 1024 * 1024;

pub struct ReadFileTool;

#[async_trait::async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn category(&self) -> Category {
        Category::Files
    }

    fn headline(&self, args: &Value) -> Option<String> {
        args.get("path").and_then(Value::as_str).map(str::to_string)
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a text file from the user's machine and return its contents \
                                (UTF-8, lossy). Large files are truncated to a byte budget.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute or relative path to read." },
                        "max_bytes": {
                            "type": "integer",
                            "description": "Optional max bytes to return (default 1048576, max 16777216)."
                        }
                    },
                    "required": ["path"]
                }
            }
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolCtx) -> Result<ToolResult> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("read_file requires a 'path' string"))?
            .to_string();
        let max_bytes = args.get("max_bytes").and_then(Value::as_u64);
        let cap = max_bytes.unwrap_or(DEFAULT_READ_BYTES).min(MAX_READ_BYTES);
        if cap == 0 {
            return Err(anyhow!("max_bytes must be > 0"));
        }

        let path_buf = PathBuf::from(&path);
        let total_bytes = tokio::fs::metadata(&path_buf).await.ok().map(|m| m.len());
        let bytes = tokio::fs::read(&path_buf)
            .await
            .with_context(|| format!("read {path}"))?;
        let cap_usize: usize = cap.try_into().unwrap_or(usize::MAX);
        let (slice, truncated) = if bytes.len() > cap_usize {
            (&bytes[..cap_usize], true)
        } else {
            (&bytes[..], false)
        };
        let mut text = String::from_utf8_lossy(slice).into_owned();
        if truncated {
            let total = total_bytes
                .map(|t| t.to_string())
                .unwrap_or_else(|| "unknown".into());
            text.push_str(&format!(
                "\n…(truncated: returned {} of {total} bytes)",
                slice.len()
            ));
        }
        Ok(ToolResult::text(text))
    }
}

pub struct WriteFileTool;

#[async_trait::async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn category(&self) -> Category {
        Category::Files
    }

    fn headline(&self, args: &Value) -> Option<String> {
        args.get("path").and_then(Value::as_str).map(str::to_string)
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Write text to a file on the user's machine. Creates parent \
                                directories by default. Set append to add to the end instead of \
                                overwriting.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to write." },
                        "content": { "type": "string", "description": "The text to write." },
                        "create_dirs": {
                            "type": "boolean",
                            "description": "Create missing parent directories (default true)."
                        },
                        "append": {
                            "type": "boolean",
                            "description": "Append instead of overwriting (default false)."
                        }
                    },
                    "required": ["path", "content"]
                }
            }
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolCtx) -> Result<ToolResult> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("write_file requires a 'path' string"))?
            .to_string();
        let content = args
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("write_file requires a 'content' string"))?
            .to_string();
        let create_dirs = args
            .get("create_dirs")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let append = args.get("append").and_then(Value::as_bool).unwrap_or(false);

        let path_buf = PathBuf::from(&path);
        let mut created_dirs = false;
        if create_dirs {
            if let Some(parent) = path_buf.parent() {
                if !parent.as_os_str().is_empty() && !parent.exists() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .with_context(|| format!("mkdir {}", parent.display()))?;
                    created_dirs = true;
                }
            }
        }
        let bytes = content.as_bytes();
        if append {
            use tokio::io::AsyncWriteExt;
            let mut f = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path_buf)
                .await
                .with_context(|| format!("open {path} for append"))?;
            f.write_all(bytes)
                .await
                .with_context(|| format!("append to {path}"))?;
            f.flush().await.ok();
        } else {
            tokio::fs::write(&path_buf, bytes)
                .await
                .with_context(|| format!("write {path}"))?;
        }
        let verb = if append { "Appended" } else { "Wrote" };
        let dirs = if created_dirs {
            " (created parent directories)"
        } else {
            ""
        };
        Ok(ToolResult::text(format!(
            "{verb} {} bytes to {path}{dirs}.",
            bytes.len()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn ctx() -> ToolCtx {
        let (tx, _rx) = mpsc::unbounded_channel();
        let web = std::sync::Arc::new(
            crate::tools::web::WebSearch::new(crate::config::WebSearchConfig::Ddg).unwrap(),
        );
        ToolCtx {
            turn: 1,
            round: 0,
            web,
            events: tx,
        }
    }

    fn tempdir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn write_then_read_roundtrip() {
        let dir = tempdir("myo-files-rw");
        let path = dir.join("sub").join("f.txt");
        let p = path.to_string_lossy().into_owned();

        let w = WriteFileTool
            .execute(json!({ "path": p, "content": "hello\nworld" }), &ctx())
            .await
            .unwrap();
        assert!(w.text.contains("Wrote 11 bytes"));
        assert!(w.text.contains("created parent directories"));

        let r = ReadFileTool
            .execute(json!({ "path": p }), &ctx())
            .await
            .unwrap();
        assert_eq!(r.text, "hello\nworld");

        WriteFileTool
            .execute(json!({ "path": p, "content": "!", "append": true }), &ctx())
            .await
            .unwrap();
        let r2 = ReadFileTool
            .execute(json!({ "path": p }), &ctx())
            .await
            .unwrap();
        assert_eq!(r2.text, "hello\nworld!");
    }

    #[tokio::test]
    async fn read_truncates_at_max_bytes() {
        let dir = tempdir("myo-files-trunc");
        let path = dir.join("big.txt");
        let p = path.to_string_lossy().into_owned();
        WriteFileTool
            .execute(json!({ "path": p, "content": "x".repeat(100) }), &ctx())
            .await
            .unwrap();
        let r = ReadFileTool
            .execute(json!({ "path": p, "max_bytes": 20 }), &ctx())
            .await
            .unwrap();
        assert!(r.text.starts_with(&"x".repeat(20)));
        assert!(r.text.contains("truncated"));
    }

    #[tokio::test]
    async fn missing_args_error() {
        assert!(ReadFileTool.execute(json!({}), &ctx()).await.is_err());
        assert!(WriteFileTool
            .execute(json!({ "path": "/tmp/x" }), &ctx())
            .await
            .is_err());
    }
}
