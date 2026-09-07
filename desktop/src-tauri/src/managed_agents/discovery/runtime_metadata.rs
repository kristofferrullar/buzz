/// Static capabilities and installation metadata for a known ACP runtime.
pub(crate) struct KnownAcpRuntime {
    pub id: &'static str,
    pub label: &'static str,
    pub commands: &'static [&'static str],
    pub aliases: &'static [&'static str],
    pub avatar_url: &'static str,
    /// Legacy MCP server binary field. Vestigial — all agents now use the bundled CLI
    /// directly. Will be removed when runtime discovery is simplified.
    pub mcp_command: Option<&'static str>,
    /// Whether to enable MCP hook tools (`_Stop`, `_PostCompact`) for this agent.
    pub mcp_hooks: bool,
    /// CLI binary that indicates partial install (e.g. `"claude"` when `claude-agent-acp` is missing).
    pub underlying_cli: Option<&'static str>,
    /// Shell commands to install the runtime CLI itself (run sequentially).
    pub cli_install_commands: &'static [&'static str],
    /// Windows-specific CLI install commands (e.g. PowerShell installers).
    /// When non-empty on Windows, these are used instead of `cli_install_commands`.
    #[allow(dead_code)] // read only on Windows via cli_install_commands_for_os()
    pub cli_install_commands_windows: &'static [&'static str],
    /// Shell commands to install the ACP adapter (run sequentially, after CLI).
    pub adapter_install_commands: &'static [&'static str],
    /// Official CLI installation documentation.
    pub cli_install_instructions_url: &'static str,
    /// ACP adapter installation documentation.
    pub adapter_install_instructions_url: &'static str,
    /// Human-readable hint about installing the CLI binary.
    pub cli_install_hint: &'static str,
    /// Human-readable hint about installing the ACP adapter.
    pub adapter_install_hint: &'static str,
    /// Harness-specific skill discovery directory (e.g. `.goose/skills`).
    /// `Some(dir)` → Buzz creates a symlink at `<nest>/<dir>/buzz-cli`
    /// pointing to the canonical `.agents/skills/buzz-cli`. `None` → this
    /// runtime reads the canonical path directly or has no skill support.
    pub skill_dir: Option<&'static str>,
    /// Whether this runtime handles model switching via ACP protocol natively.
    /// Currently unused — env var injection runs unconditionally regardless of
    /// this value. Retained as scaffolding for when ACP model switching matures.
    #[allow(dead_code)]
    pub supports_acp_model_switching: bool,
    pub model_env_var: Option<&'static str>,
    pub provider_env_var: Option<&'static str>,
    pub provider_locked: bool,
    pub default_env: &'static [(&'static str, &'static str)],
    pub config_file_path: Option<&'static str>,
    #[allow(dead_code)] // reserved for format-based dispatch when readers are unified
    pub config_file_format: Option<&'static str>,
    pub supports_acp_native_config: bool, // tier 1a: config/read+write
    pub thinking_env_var: Option<&'static str>,
    /// Env var for normalizing `max_output_tokens`. `None` when the harness
    /// does not have a first-class env var for this field (config-file only).
    pub max_tokens_env_var: Option<&'static str>,
    /// Env var for normalizing `context_limit`. `None` when not applicable.
    pub context_limit_env_var: Option<&'static str>,
    /// Env var for normalizing `max_rounds`. `None` when not applicable.
    pub max_rounds_env_var: Option<&'static str>,
    /// Normalized field keys that must be set for this harness to function.
    /// Used by the config bridge to mark fields as required in the UI.
    /// Keys match the camelCase names used in `NormalizedConfig` (e.g. "model", "provider").
    pub required_normalized_fields: &'static [&'static str],
    /// Human-readable hint shown in Doctor when the runtime is available but not
    /// authenticated. `None` for runtimes that have no login step (goose, buzz-agent).
    pub login_hint: Option<&'static str>,
    /// CLI args for probing authentication status. `args[0]` is the binary name;
    /// the remainder are the subcommand. `None` for runtimes with no login step.
    pub auth_probe_args: Option<&'static [&'static str]>,
}

impl KnownAcpRuntime {
    /// Return the CLI install commands for the current platform.
    ///
    /// On Windows, returns `cli_install_commands_windows` when non-empty,
    /// falling back to the default `cli_install_commands`. On other platforms
    /// always returns `cli_install_commands`.
    pub fn cli_install_commands_for_os(&self) -> &[&str] {
        #[cfg(windows)]
        {
            if !self.cli_install_commands_windows.is_empty() {
                return self.cli_install_commands_windows;
            }
        }
        self.cli_install_commands
    }
}

/// Resolve `cursor-agent` when it is not on PATH (versioned Cursor install dirs).
pub(crate) fn resolve_cursor_agent_command(command: &str) -> Option<std::path::PathBuf> {
    if super::normalize_command_identity(command) != "cursor-agent" {
        return None;
    }

    let home = dirs::home_dir()?;
    let local_bin = home.join(".local/bin/cursor-agent");
    if is_executable_file(&local_bin) {
        return Some(local_bin);
    }

    for versions_dir in cursor_agent_version_dirs(&home) {
        if let Some(path) = latest_cursor_agent_in_versions_dir(&versions_dir) {
            return Some(path);
        }
    }

    None
}

fn cursor_agent_version_dirs(home: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut dirs = vec![home.join(".local/share/cursor-agent/versions")];
    #[cfg(target_os = "macos")]
    {
        dirs.push(
            home.join("Library/Application Support/Cursor/User/globalStorage/anysphere.cursor-agent-worker/agent-cli/.local/share/cursor-agent/versions"),
        );
    }
    dirs
}

fn latest_cursor_agent_in_versions_dir(
    versions_dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let mut latest: Option<(String, std::path::PathBuf)> = None;
    let entries = std::fs::read_dir(versions_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let candidate = path.join("cursor-agent");
        if !is_executable_file(&candidate) {
            continue;
        }
        let version_name = entry.file_name().to_string_lossy().into_owned();
        if latest.as_ref().is_none_or(|(name, _)| version_name > *name) {
            latest = Some((version_name, candidate));
        }
    }
    latest.map(|(_, path)| path)
}

/// Login-shell PATH lookup, then well-known `cursor-agent` install dirs.
pub(crate) fn resolve_login_shell_or_cursor(command: &str) -> Option<std::path::PathBuf> {
    super::find_via_login_shell(command).or_else(|| resolve_cursor_agent_command(command))
}

fn is_executable_file(path: &std::path::Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::known_acp_runtime_exact;
    use super::super::{clear_resolve_cache, normalize_agent_args};
    use super::resolve_cursor_agent_command;

    #[test]
    fn normalizes_cursor_agent_args_to_acp() {
        assert_eq!(
            normalize_agent_args("cursor-agent", Vec::new()),
            vec!["acp".to_string()]
        );
        assert_eq!(
            normalize_agent_args("cursor-agent", vec!["acp".into()]),
            vec!["acp".to_string()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_cursor_agent_command_finds_versioned_install_dir() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = crate::managed_agents::lock_path_mutex();
        clear_resolve_cache();

        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let versions_dir = home.join(".local/share/cursor-agent/versions/2026.07.23-e383d2b");
        std::fs::create_dir_all(&versions_dir).expect("create versions dir");

        let binary = versions_dir.join("cursor-agent");
        std::fs::write(&binary, "#!/bin/sh\necho cursor-agent\n").expect("write binary");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
            .expect("chmod binary");

        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);

        let resolved = resolve_cursor_agent_command("cursor-agent");

        if let Some(value) = previous_home {
            std::env::set_var("HOME", value);
        } else {
            std::env::remove_var("HOME");
        }

        assert_eq!(resolved, Some(binary));
    }

    #[test]
    fn resolve_cursor_agent_command_ignores_other_commands() {
        assert!(resolve_cursor_agent_command("goose").is_none());
    }

    #[test]
    fn vendor_metadata_distinguishes_cli_and_adapter_guidance() {
        let goose = known_acp_runtime_exact("goose").unwrap();
        assert_eq!(
            goose.cli_install_instructions_url,
            "https://goose-docs.ai/docs/getting-started/installation/"
        );
        assert!(goose.adapter_install_instructions_url.is_empty());
        assert!(goose.cli_install_hint.contains("Goose CLI"));
        assert!(goose
            .cli_install_commands_windows
            .iter()
            .any(|command| command.contains("raw.githubusercontent.com/aaif-goose/goose/main")));
        assert!(goose
            .cli_install_commands_windows
            .iter()
            .any(|command| command.contains("$env:CONFIGURE='false'")));

        let claude = known_acp_runtime_exact("claude").unwrap();
        assert_eq!(
            claude.cli_install_instructions_url,
            "https://code.claude.com/docs/en/getting-started"
        );
        assert!(claude
            .adapter_install_instructions_url
            .contains("claude-agent-acp"));
        assert!(claude.cli_install_hint.contains("Claude Code CLI"));

        let codex = known_acp_runtime_exact("codex").unwrap();
        assert_eq!(
            codex.cli_install_instructions_url,
            "https://developers.openai.com/codex/cli/"
        );
        assert!(codex.adapter_install_instructions_url.contains("codex-acp"));
        assert!(codex.cli_install_hint.contains("Codex CLI"));
    }
}
