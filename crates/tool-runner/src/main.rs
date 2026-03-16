use anyhow::Result;
use clap::Parser;
use memory_core::{MemoryEngine as _, MemoryScope, MuninnConfig, MuninnRestEngine, VaultResolver};
use philotic_client::{is_ipc_disconnect, GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::json;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use tracing::{info, warn};

fn local_node_id() -> String {
    std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string())
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceRunnerConfig {
    default_workspace_ref: Option<String>,
    allowed_tools: Vec<String>,
    max_read_bytes: usize,
    max_search_results: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Deserialize)]
struct WorkspaceRunnerBaseConfig {
    #[serde(default)]
    default_workspace_ref: Option<String>,
    #[serde(default)]
    allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    max_read_bytes: Option<usize>,
    #[serde(default)]
    max_search_results: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Deserialize)]
struct WorkspaceRunnerOverlay {
    #[serde(default)]
    workspace_ref: Option<String>,
    #[serde(default)]
    allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    max_read_bytes: Option<usize>,
    #[serde(default)]
    max_search_results: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EffectiveWorkspaceRunnerConfig {
    workspace_ref: Option<String>,
    allowed_tools: Vec<String>,
    max_read_bytes: usize,
    max_search_results: usize,
}

impl WorkspaceRunnerConfig {
    fn from_env() -> Self {
        let default_workspace_ref = std::env::var("PHILOTIC_WORKSPACE_RUNNER_ROOT")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let allowed_tools = std::env::var("PHILOTIC_WORKSPACE_ALLOWED_TOOLS")
            .ok()
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|tool| !tool.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .filter(|tools| !tools.is_empty())
            .unwrap_or_else(|| {
                vec![
                    "workspace.list".into(),
                    "workspace.read".into(),
                    "workspace.search".into(),
                ]
            });
        let max_read_bytes = std::env::var("PHILOTIC_WORKSPACE_MAX_READ_BYTES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(256 * 1024);
        let max_search_results = std::env::var("PHILOTIC_WORKSPACE_MAX_SEARCH_RESULTS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(50);

        Self {
            default_workspace_ref,
            allowed_tools,
            max_read_bytes,
            max_search_results,
        }
    }

    fn apply_overlay(&self, overlay: WorkspaceRunnerOverlay) -> EffectiveWorkspaceRunnerConfig {
        let allowed_tools = match overlay.allowed_tools {
            Some(overlay_tools) => self
                .allowed_tools
                .iter()
                .filter(|tool| {
                    overlay_tools
                        .iter()
                        .any(|overlay_tool| overlay_tool == *tool)
                })
                .cloned()
                .collect::<Vec<_>>(),
            None => self.allowed_tools.clone(),
        };

        EffectiveWorkspaceRunnerConfig {
            workspace_ref: overlay
                .workspace_ref
                .or_else(|| self.default_workspace_ref.clone()),
            allowed_tools,
            max_read_bytes: overlay
                .max_read_bytes
                .map(|limit| limit.min(self.max_read_bytes))
                .unwrap_or(self.max_read_bytes),
            max_search_results: overlay
                .max_search_results
                .map(|limit| limit.min(self.max_search_results))
                .unwrap_or(self.max_search_results),
        }
    }

    fn apply_base_config(&self, base: WorkspaceRunnerBaseConfig) -> Self {
        let allowed_tools = base
            .allowed_tools
            .map(|tools| {
                self.allowed_tools
                    .iter()
                    .filter(|tool| tools.iter().any(|configured| configured == *tool))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| self.allowed_tools.clone());

        Self {
            default_workspace_ref: base
                .default_workspace_ref
                .or_else(|| self.default_workspace_ref.clone()),
            allowed_tools,
            max_read_bytes: base
                .max_read_bytes
                .map(|limit| limit.min(self.max_read_bytes))
                .unwrap_or(self.max_read_bytes),
            max_search_results: base
                .max_search_results
                .map(|limit| limit.min(self.max_search_results))
                .unwrap_or(self.max_search_results),
        }
    }
}

fn resolve_workspace_root(workspace_ref: Option<&str>) -> PathBuf {
    let current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(workspace_ref) = workspace_ref
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return current;
    };

    if let Some(path) = workspace_ref.strip_prefix("file://") {
        return PathBuf::from(path);
    }

    if workspace_ref.starts_with("workspace://") {
        return current;
    }

    PathBuf::from(workspace_ref)
}

fn resolve_workspace_path(root: &Path, requested: &str) -> Result<PathBuf, String> {
    let requested_path = Path::new(requested.trim());
    if requested_path.as_os_str().is_empty() {
        return Ok(root.to_path_buf());
    }

    if requested_path.is_absolute() {
        return Err("absolute paths are not allowed for workspace tools".into());
    }

    if requested_path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("path traversal is not allowed for workspace tools".into());
    }

    Ok(root.join(requested_path))
}

fn parse_workspace_runner_overlay(task: &serde_json::Value) -> WorkspaceRunnerOverlay {
    let mut overlay = task
        .get("task_runner_overlay")
        .cloned()
        .and_then(|value| serde_json::from_value::<WorkspaceRunnerOverlay>(value).ok())
        .unwrap_or_default();

    if overlay.workspace_ref.is_none() {
        overlay.workspace_ref = task
            .get("workspace_ref")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
    }

    overlay
}

fn parse_workspace_runner_base_config(task: &serde_json::Value) -> WorkspaceRunnerBaseConfig {
    task.get("task_runner_config")
        .cloned()
        .and_then(|value| serde_json::from_value::<WorkspaceRunnerBaseConfig>(value).ok())
        .unwrap_or_default()
}

fn execute_tool(
    config: &EffectiveWorkspaceRunnerConfig,
    tool_name: &str,
    arguments: &serde_json::Value,
) -> String {
    if !config
        .allowed_tools
        .iter()
        .any(|allowed| allowed == tool_name)
    {
        return format!(
            "{} error: tool is not allowed by workspace runner policy",
            tool_name
        );
    }

    match tool_name {
        "echo" => arguments
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        "workspace.list" => {
            let root = resolve_workspace_root(config.workspace_ref.as_deref());
            let requested = arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(".");
            let path = match resolve_workspace_path(&root, requested) {
                Ok(path) => path,
                Err(err) => return format!("workspace.list error: {err}"),
            };
            let mut entries = match fs::read_dir(&path) {
                Ok(entries) => entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| {
                        let file_name = entry.file_name().to_string_lossy().to_string();
                        if entry.path().is_dir() {
                            format!("{file_name}/")
                        } else {
                            file_name
                        }
                    })
                    .collect::<Vec<_>>(),
                Err(err) => return format!("workspace.list error: {err}"),
            };
            entries.sort();
            entries.join("\n")
        }
        "workspace.read" => {
            let root = resolve_workspace_root(config.workspace_ref.as_deref());
            let Some(requested) = arguments.get("path").and_then(serde_json::Value::as_str) else {
                return "workspace.read error: missing `path`".into();
            };
            let path = match resolve_workspace_path(&root, requested) {
                Ok(path) => path,
                Err(err) => return format!("workspace.read error: {err}"),
            };
            let metadata = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(err) => return format!("workspace.read error: {err}"),
            };
            if metadata.len() as usize > config.max_read_bytes {
                return format!(
                    "workspace.read error: file exceeds max_read_bytes ({})",
                    config.max_read_bytes
                );
            }
            match fs::read_to_string(&path) {
                Ok(contents) => contents,
                Err(err) => format!("workspace.read error: {err}"),
            }
        }
        "workspace.search" => {
            let root = resolve_workspace_root(config.workspace_ref.as_deref());
            let query = arguments
                .get("query")
                .or_else(|| arguments.get("pattern"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let Some(query) = query else {
                return "workspace.search error: missing `query`".into();
            };
            let requested = arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(".");
            let path = match resolve_workspace_path(&root, requested) {
                Ok(path) => path,
                Err(err) => return format!("workspace.search error: {err}"),
            };
            match search_workspace(
                &path,
                query,
                config.max_read_bytes,
                config.max_search_results,
            ) {
                Ok(results) if results.is_empty() => {
                    format!("workspace.search: no matches for `{query}`")
                }
                Ok(results) => results.join("\n"),
                Err(err) => format!("workspace.search error: {err}"),
            }
        }
        _ => "unsupported tool".into(),
    }
}

fn search_workspace(
    root: &Path,
    query: &str,
    max_read_bytes: usize,
    max_search_results: usize,
) -> Result<Vec<String>, String> {
    fn visit(
        path: &Path,
        root: &Path,
        query: &str,
        max_read_bytes: usize,
        max_search_results: usize,
        results: &mut Vec<String>,
    ) -> Result<(), String> {
        if results.len() >= max_search_results {
            return Ok(());
        }

        let metadata = fs::metadata(path).map_err(|err| err.to_string())?;
        if metadata.is_dir() {
            let mut entries = fs::read_dir(path)
                .map_err(|err| err.to_string())?
                .filter_map(|entry| entry.ok())
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                visit(
                    &entry.path(),
                    root,
                    query,
                    max_read_bytes,
                    max_search_results,
                    results,
                )?;
                if results.len() >= max_search_results {
                    break;
                }
            }
            return Ok(());
        }

        if !metadata.is_file() || metadata.len() as usize > max_read_bytes {
            return Ok(());
        }

        let relative = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        let file_name_matches = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.contains(query))
            .unwrap_or(false);
        if file_name_matches {
            results.push(format!("{relative}: [filename match]"));
            if results.len() >= max_search_results {
                return Ok(());
            }
        }

        let mut file = fs::File::open(path).map_err(|err| err.to_string())?;
        let mut contents = String::new();
        if file.read_to_string(&mut contents).is_err() {
            return Ok(());
        }

        for (idx, line) in contents.lines().enumerate() {
            if !line.contains(query) {
                continue;
            }
            results.push(format!("{relative}:{}: {}", idx + 1, line.trim()));
            if results.len() >= max_search_results {
                break;
            }
        }

        Ok(())
    }

    let mut results = Vec::new();
    visit(
        root,
        root,
        query,
        max_read_bytes,
        max_search_results,
        &mut results,
    )?;
    Ok(results)
}

// ──── Memory tool helpers ─────────────────────────────────────────────────────

fn parse_scope(s: Option<&str>, session_id: &str) -> MemoryScope {
    match s {
        Some("user")    => MemoryScope::SharedUser,
        Some("session") => MemoryScope::Session(session_id.to_string()),
        _               => MemoryScope::SelfOnly,
    }
}

fn memory_engine(config: &MuninnConfig, agent_id: &str, user_id: &str) -> MuninnRestEngine {
    MuninnRestEngine::new(
        config.clone(),
        VaultResolver {
            agent_id: agent_id.to_string(),
            user_id: user_id.to_string(),
        },
    )
}

async fn execute_memory_tool(
    config: &MuninnConfig,
    tool_name: &str,
    arguments: &serde_json::Value,
    agent_id: &str,
    user_id: &str,
    session_id: &str,
) -> String {
    let engine = memory_engine(config, agent_id, user_id);
    let scope = parse_scope(
        arguments.get("scope").and_then(|v| v.as_str()),
        session_id,
    );

    match tool_name {
        "memory.recall" => {
            let context = match arguments.get("context").and_then(|v| v.as_str()) {
                Some(c) => c,
                None => return "memory.recall error: missing `context`".into(),
            };
            let max_results = arguments
                .get("max_results")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);

            match engine.activate(context, scope, max_results).await {
                Ok(result) if result.engrams.is_empty() => {
                    "memory.recall: no memories found".into()
                }
                Ok(result) => result
                    .engrams
                    .iter()
                    .map(|e| format!("[{}] {}: {}", e.id, e.concept, e.content))
                    .collect::<Vec<_>>()
                    .join("\n"),
                Err(e) => format!("memory.recall error: {e}"),
            }
        }

        "memory.remember" => {
            let concept = match arguments.get("concept").and_then(|v| v.as_str()) {
                Some(c) => c,
                None => return "memory.remember error: missing `concept`".into(),
            };
            let content = match arguments.get("content").and_then(|v| v.as_str()) {
                Some(c) => c,
                None => return "memory.remember error: missing `content`".into(),
            };
            let tags: Vec<String> = arguments
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|t| t.as_str().map(str::to_string)).collect())
                .unwrap_or_default();

            match engine.remember(scope, concept, content, tags).await {
                Ok(r) => format!("stored: {}", r.id),
                Err(e) => format!("memory.remember error: {e}"),
            }
        }

        "memory.forget" => {
            let id = match arguments.get("id").and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                None => return "memory.forget error: missing `id`".into(),
            };
            match engine.forget(&id).await {
                Ok(()) => "forgotten".into(),
                Err(e) => format!("memory.forget error: {e}"),
            }
        }

        "memory.link" => {
            let from_id = match arguments.get("from_id").and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                None => return "memory.link error: missing `from_id`".into(),
            };
            let to_id = match arguments.get("to_id").and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                None => return "memory.link error: missing `to_id`".into(),
            };
            let kind = match arguments.get("kind").and_then(|v| v.as_str()).unwrap_or("related") {
                "contradicts"  => memory_core::LinkKind::Contradicts,
                "supersedes"   => memory_core::LinkKind::Supersedes,
                "supports"     => memory_core::LinkKind::Supports,
                "derived_from" => memory_core::LinkKind::DerivedFrom,
                "related"      => memory_core::LinkKind::Related,
                other          => memory_core::LinkKind::Custom(other.to_string()),
            };
            match engine.link(&from_id, &to_id, kind).await {
                Ok(()) => "linked".into(),
                Err(e) => format!("memory.link error: {e}"),
            }
        }

        _ => format!("{tool_name}: unsupported memory tool"),
    }
}

// ──── Entry point ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let _args = Args::parse();
    let workspace_config = WorkspaceRunnerConfig::from_env();

    let identity = GuestIdentity {
        guest_id: "tool-runner-01".into(),
        role: "tool".into(),
        supported_tools: vec![
            "echo".into(),
            "workspace.list".into(),
            "workspace.read".into(),
            "workspace.search".into(),
            "memory.recall".into(),
            "memory.remember".into(),
            "memory.forget".into(),
            "memory.link".into(),
        ],
    };
    let mut ipc_client = PhiloticClient::connect(identity).await?;

    for role in &[
        "tool.echo",
        "tool.workspace.list",
        "tool.workspace.read",
        "tool.workspace.search",
        "tool.memory.recall",
        "tool.memory.remember",
        "tool.memory.forget",
        "tool.memory.link",
    ] {
        let _ = ipc_client
            .send_request(IpcRequest::SubscribeInbox { role: role.to_string() })
            .await?;
    }

    // Fetch MuninnDB config from the hotel — None if memory is not configured.
    let muninn_config: Option<MuninnConfig> =
        match ipc_client.send_request(IpcRequest::FetchMemoryConfig).await {
            Ok(IpcResponse::MemoryConfig { config_json: Some(json) }) => {
                match serde_json::from_str::<MuninnConfig>(&json) {
                    Ok(cfg) => {
                        info!(endpoint = %cfg.base_url, vaults = cfg.vault_tokens.len(), "Memory tools enabled");
                        Some(cfg)
                    }
                    Err(e) => { warn!("Failed to parse MuninnConfig: {e}"); None }
                }
            }
            Ok(IpcResponse::MemoryConfig { config_json: None }) => {
                info!("Memory tools disabled — hotel has no MuninnDB config");
                None
            }
            _ => { warn!("Unexpected response to FetchMemoryConfig"); None }
        };

    info!("Listening for tool execution tasks...");

    loop {
        match tokio::time::timeout(Duration::from_secs(5), ipc_client.recv_task()).await {
            Ok(Ok(IpcResponse::InboundTask { task_json, .. })) => {
                let Ok(task) = serde_json::from_str::<serde_json::Value>(&task_json) else {
                    warn!("Could not parse tool task payload");
                    continue;
                };

                if task.get("action").and_then(serde_json::Value::as_str) != Some("execute_tool") {
                    continue;
                }

                let tool_name = task
                    .get("tool_name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let arguments = task.get("arguments").cloned().unwrap_or_else(|| json!({}));
                let session_id = task
                    .get("session_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let turn_id = task
                    .get("turn_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let chat_id = task
                    .get("chat_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let local_node_id = local_node_id();
                let reply_to = task
                    .get("reply_to")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&local_node_id)
                    .to_string();
                let reply_role = task
                    .get("reply_role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("agent")
                    .to_string();
                let final_reply_to = task
                    .get("final_reply_to")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&local_node_id)
                    .to_string();
                let final_reply_role = task
                    .get("final_reply_role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("membrane")
                    .to_string();
                let runner_base_config = parse_workspace_runner_base_config(&task);
                let runner_overlay = parse_workspace_runner_overlay(&task);
                let effective_config = workspace_config
                    .apply_base_config(runner_base_config)
                    .apply_overlay(runner_overlay);

                let result_content = if tool_name.starts_with("memory.") {
                    let agent_id = task
                        .get("agent_id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown");
                    let user_id = task
                        .get("user_id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(agent_id);
                    match &muninn_config {
                        Some(cfg) => {
                            execute_memory_tool(cfg, &tool_name, &arguments, agent_id, user_id, &session_id).await
                        }
                        None => format!("{tool_name} error: memory not configured on this hotel"),
                    }
                } else {
                    execute_tool(&effective_config, &tool_name, &arguments)
                };

                ipc_client
                    .send_request(IpcRequest::EmitTask {
                        target_node: reply_to,
                        target_role: reply_role,
                        target_guest_id: None,
                        task_json: json!({
                            "action": "tool_result",
                            "session_id": session_id,
                            "turn_id": turn_id,
                            "chat_id": chat_id,
                            "tool_name": tool_name,
                            "content": result_content,
                            "final_reply_to": final_reply_to,
                            "final_reply_role": final_reply_role
                        })
                        .to_string(),
                    })
                    .await?;
            }
            Ok(Ok(_)) => {}
            Ok(Err(err)) => {
                if is_ipc_disconnect(&err) {
                    info!("Hotel IPC disconnected; tool-runner exiting.");
                    return Ok(());
                }
                warn!("IPC Recv error: {}", err);
            }
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        execute_tool, parse_workspace_runner_base_config, parse_workspace_runner_overlay,
        search_workspace, EffectiveWorkspaceRunnerConfig, WorkspaceRunnerConfig,
        WorkspaceRunnerOverlay,
    };
    use serde_json::json;
    use std::fs;

    #[test]
    fn workspace_list_returns_directory_entries() {
        let temp = tempfile::tempdir().expect("temp dir");
        fs::write(temp.path().join("alpha.txt"), "hello").expect("write file");
        fs::create_dir(temp.path().join("nested")).expect("mkdir");

        let config = EffectiveWorkspaceRunnerConfig {
            workspace_ref: temp.path().to_str().map(str::to_string),
            allowed_tools: vec!["workspace.list".into()],
            max_read_bytes: 256 * 1024,
            max_search_results: 50,
        };
        let output = execute_tool(&config, "workspace.list", &json!({ "path": "." }));
        assert!(output.contains("alpha.txt"));
        assert!(output.contains("nested/"));
    }

    #[test]
    fn workspace_read_returns_file_contents() {
        let temp = tempfile::tempdir().expect("temp dir");
        fs::write(temp.path().join("note.txt"), "remember this").expect("write file");

        let config = EffectiveWorkspaceRunnerConfig {
            workspace_ref: temp.path().to_str().map(str::to_string),
            allowed_tools: vec!["workspace.read".into()],
            max_read_bytes: 256 * 1024,
            max_search_results: 50,
        };
        let output = execute_tool(&config, "workspace.read", &json!({ "path": "note.txt" }));
        assert_eq!(output, "remember this");
    }

    #[test]
    fn workspace_tools_reject_parent_traversal() {
        let temp = tempfile::tempdir().expect("temp dir");
        let config = EffectiveWorkspaceRunnerConfig {
            workspace_ref: temp.path().to_str().map(str::to_string),
            allowed_tools: vec!["workspace.read".into()],
            max_read_bytes: 256 * 1024,
            max_search_results: 50,
        };
        let output = execute_tool(
            &config,
            "workspace.read",
            &json!({ "path": "../secret.txt" }),
        );
        assert!(output.contains("path traversal"));
    }

    #[test]
    fn workspace_search_returns_matching_lines() {
        let temp = tempfile::tempdir().expect("temp dir");
        fs::write(
            temp.path().join("note.txt"),
            "alpha\nremember this line\nomega\n",
        )
        .expect("write file");

        let config = EffectiveWorkspaceRunnerConfig {
            workspace_ref: temp.path().to_str().map(str::to_string),
            allowed_tools: vec!["workspace.search".into()],
            max_read_bytes: 256 * 1024,
            max_search_results: 50,
        };
        let output = execute_tool(
            &config,
            "workspace.search",
            &json!({ "path": ".", "query": "remember" }),
        );
        assert!(output.contains("note.txt:2: remember this line"));
    }

    #[test]
    fn workspace_search_requires_query() {
        let temp = tempfile::tempdir().expect("temp dir");
        let config = EffectiveWorkspaceRunnerConfig {
            workspace_ref: temp.path().to_str().map(str::to_string),
            allowed_tools: vec!["workspace.search".into()],
            max_read_bytes: 256 * 1024,
            max_search_results: 50,
        };
        let output = execute_tool(&config, "workspace.search", &json!({ "path": "." }));
        assert!(output.contains("missing `query`"));
    }

    #[test]
    fn task_runner_overlay_workspace_ref_takes_precedence() {
        let task = json!({
            "workspace_ref": "workspace://legacy",
            "task_runner_overlay": {
                "workspace_ref": "workspace://overlay"
            }
        });

        assert_eq!(
            parse_workspace_runner_overlay(&task)
                .workspace_ref
                .as_deref(),
            Some("workspace://overlay")
        );
    }

    #[test]
    fn task_runner_base_config_can_narrow_env_defaults() {
        let env_base = WorkspaceRunnerConfig {
            default_workspace_ref: Some("workspace://env".into()),
            allowed_tools: vec![
                "workspace.list".into(),
                "workspace.read".into(),
                "workspace.search".into(),
            ],
            max_read_bytes: 4096,
            max_search_results: 20,
        };
        let task = json!({
            "task_runner_config": {
                "default_workspace_ref": "workspace://session",
                "allowed_tools": ["workspace.read", "workspace.search"],
                "max_read_bytes": 1024,
                "max_search_results": 5
            }
        });

        let effective = env_base.apply_base_config(parse_workspace_runner_base_config(&task));

        assert_eq!(
            effective.default_workspace_ref.as_deref(),
            Some("workspace://session")
        );
        assert_eq!(
            effective.allowed_tools,
            vec!["workspace.read", "workspace.search"]
        );
        assert_eq!(effective.max_read_bytes, 1024);
        assert_eq!(effective.max_search_results, 5);
    }

    #[test]
    fn workspace_runner_overlay_can_only_narrow_limits() {
        let base = WorkspaceRunnerConfig {
            default_workspace_ref: Some("workspace://base".into()),
            allowed_tools: vec!["workspace.read".into(), "workspace.search".into()],
            max_read_bytes: 4096,
            max_search_results: 20,
        };
        let effective = base.apply_overlay(WorkspaceRunnerOverlay {
            workspace_ref: Some("workspace://overlay".into()),
            allowed_tools: Some(vec!["workspace.search".into()]),
            max_read_bytes: Some(1024),
            max_search_results: Some(5),
        });

        assert_eq!(
            effective.workspace_ref.as_deref(),
            Some("workspace://overlay")
        );
        assert_eq!(effective.allowed_tools, vec!["workspace.search"]);
        assert_eq!(effective.max_read_bytes, 1024);
        assert_eq!(effective.max_search_results, 5);
    }

    #[test]
    fn workspace_runner_rejects_tools_outside_overlay_policy() {
        let config = EffectiveWorkspaceRunnerConfig {
            workspace_ref: Some("workspace://main".into()),
            allowed_tools: vec!["workspace.read".into()],
            max_read_bytes: 256 * 1024,
            max_search_results: 50,
        };

        let output = execute_tool(&config, "workspace.search", &json!({ "query": "needle" }));
        assert!(output.contains("not allowed by workspace runner policy"));
    }

    #[test]
    fn workspace_read_respects_max_read_bytes() {
        let temp = tempfile::tempdir().expect("temp dir");
        fs::write(temp.path().join("note.txt"), "remember this").expect("write file");

        let config = EffectiveWorkspaceRunnerConfig {
            workspace_ref: temp.path().to_str().map(str::to_string),
            allowed_tools: vec!["workspace.read".into()],
            max_read_bytes: 4,
            max_search_results: 50,
        };

        let output = execute_tool(&config, "workspace.read", &json!({ "path": "note.txt" }));
        assert!(output.contains("exceeds max_read_bytes"));
    }

    #[test]
    fn workspace_search_respects_max_search_results() {
        let temp = tempfile::tempdir().expect("temp dir");
        fs::write(
            temp.path().join("note.txt"),
            "hit one\nhit two\nhit three\n",
        )
        .expect("write file");

        let output = search_workspace(temp.path(), "hit", 256 * 1024, 2).expect("search");
        assert_eq!(output.len(), 2);
    }
}
