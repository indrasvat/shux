# 039 — Plugin Permissions and Sandbox

**Status:** Pending
**Depends On:** 038
**Parallelizable With:** 040

---

## Problem

Plugins run third-party code inside the daemon process. Without a permission and sandbox system, any plugin could read the filesystem, inject keystrokes, spawn processes, or exfiltrate data. PRD section 7.1 (goal 2) states: "Safe by default. A theme plugin cannot read your filesystem. A status-bar plugin cannot send network requests." This task implements the enforcement layer that makes those guarantees real.

The sandbox has four layers:
1. **Permission checks**: Before every host function call, the host checks the plugin's declared permissions from `plugin.toml`. Undeclared capabilities are denied with a clear error message.
2. **Memory limits**: wasmtime's `ResourceLimiter` trait caps per-plugin memory growth (default 16 MB).
3. **CPU limits**: Epoch interruption kills plugins that run for more than 100ms (~10% overhead).
4. **WASI filesystem sandbox**: Only paths listed in `fs_read`/`fs_write` are accessible via WASI capabilities.
5. **Subprocess execution**: The `run_subprocess` permission gates direct process invocation with scrubbed environment, CWD restriction, 30s timeout, and no shell.

Without this task, all plugins have full access to all host functions, making the permission declarations in `plugin.toml` purely cosmetic.

## PRD Reference

- **section 7.1** goal 2 — Safe by default: Wasm sandbox + capability-based permissions
- **section 7.4** — `plugin.toml` `[permissions]` section: events, read_pane_output, send_keys, manage_panes, manage_sessions, api_extensions, run_subprocess, fs_read, fs_write, network, clipboard, intercept_events, override_commands
- **section 7.5** — Graduated control plane: Read (no perms), Display (no perms), Control (manage_*), Extend (api_extensions, run_subprocess)
- **section 13** — Security: plugin sandbox mitigations (ResourceLimiter, epoch interruption, WASI sandbox, scrubbed environment for subprocess)
- **section 14.1** — Performance: epoch interruption ~10% overhead, plugin call p99 <= 5ms, kill at 100ms

---

## Files to Create

- `crates/shux-plugin/src/permissions.rs` — Permission checking logic, permission-denied error formatting
- `crates/shux-plugin/src/sandbox.rs` — ResourceLimiter implementation, WASI capability configuration, subprocess sandbox
- `crates/shux-plugin/src/subprocess.rs` — Sandboxed subprocess execution (direct invocation, scrubbed env, timeout)

## Files to Modify

- `crates/shux-plugin/src/lib.rs` — Export new modules
- `crates/shux-plugin/src/wasm.rs` — Integrate ResourceLimiter into Store creation
- `crates/shux-plugin/src/host.rs` — Add permission checks to host function stubs
- `crates/shux-plugin/Cargo.toml` — Add dependencies if needed (`glob` for fs path matching)

---

## Execution Steps

### Step 1: Implement the permission checker

The permission checker is called before every host function execution. It consults the plugin's `PluginPermissions` (from `plugin.toml`) to decide whether the call is allowed.

```rust
// crates/shux-plugin/src/permissions.rs

use crate::manifest::PluginPermissions;

/// Permission required for a host function call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Permission {
    /// No permission needed (queries, display functions).
    None,
    /// Requires read_pane_output permission.
    ReadPaneOutput,
    /// Requires send_keys permission.
    SendKeys,
    /// Requires manage_panes permission.
    ManagePanes,
    /// Requires manage_sessions permission.
    ManageSessions,
    /// Requires api_extensions permission.
    ApiExtensions,
    /// Requires clipboard permission.
    Clipboard,
    /// Requires run_subprocess permission.
    RunSubprocess,
    /// Requires fs_read permission for a specific path.
    FsRead(String),
    /// Requires fs_write permission for a specific path.
    FsWrite(String),
    /// Requires network permission.
    Network,
}

/// Result of a permission check.
#[derive(Debug)]
pub enum PermissionCheck {
    /// The call is allowed.
    Allowed,
    /// The call is denied. Includes the required permission name for
    /// the error message.
    Denied(PermissionDenied),
}

/// Details about a denied permission.
#[derive(Debug, Clone)]
pub struct PermissionDenied {
    /// The permission that was required.
    pub required: String,
    /// The host function that was called.
    pub function: String,
    /// Human-readable explanation for the plugin developer.
    pub message: String,
}

impl std::fmt::Display for PermissionDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Permission denied: {} requires '{}' permission. {}",
            self.function, self.required, self.message
        )
    }
}

impl std::error::Error for PermissionDenied {}

/// Check whether a plugin has permission to call a host function.
///
/// PRD section 7.5 defines four tiers:
/// - Tier 1 (Read): get-*, list-*, get-config — no permissions
/// - Tier 2 (Display): set-status-segment, set-badge, emit-event, overlays — no permissions
/// - Tier 3 (Control): create-pane, send-keys, focus-pane, etc. — manage_panes, send_keys, manage_sessions
/// - Tier 4 (Extend): register-api-method, register-command-override, run-subprocess — api_extensions, run_subprocess
pub fn check_permission(
    permissions: &PluginPermissions,
    function_name: &str,
    required: &Permission,
) -> PermissionCheck {
    match required {
        Permission::None => PermissionCheck::Allowed,

        Permission::ReadPaneOutput => {
            if permissions.read_pane_output {
                PermissionCheck::Allowed
            } else {
                PermissionCheck::Denied(PermissionDenied {
                    required: "read_pane_output".to_string(),
                    function: function_name.to_string(),
                    message: "Add 'read_pane_output = true' to [permissions] in plugin.toml".to_string(),
                })
            }
        }

        Permission::SendKeys => {
            if permissions.send_keys {
                PermissionCheck::Allowed
            } else {
                PermissionCheck::Denied(PermissionDenied {
                    required: "send_keys".to_string(),
                    function: function_name.to_string(),
                    message: "Add 'send_keys = true' to [permissions] in plugin.toml. Note: this is a security-sensitive permission.".to_string(),
                })
            }
        }

        Permission::ManagePanes => {
            if permissions.manage_panes {
                PermissionCheck::Allowed
            } else {
                PermissionCheck::Denied(PermissionDenied {
                    required: "manage_panes".to_string(),
                    function: function_name.to_string(),
                    message: "Add 'manage_panes = true' to [permissions] in plugin.toml".to_string(),
                })
            }
        }

        Permission::ManageSessions => {
            if permissions.manage_sessions {
                PermissionCheck::Allowed
            } else {
                PermissionCheck::Denied(PermissionDenied {
                    required: "manage_sessions".to_string(),
                    function: function_name.to_string(),
                    message: "Add 'manage_sessions = true' to [permissions] in plugin.toml".to_string(),
                })
            }
        }

        Permission::ApiExtensions => {
            if permissions.api_extensions {
                PermissionCheck::Allowed
            } else {
                PermissionCheck::Denied(PermissionDenied {
                    required: "api_extensions".to_string(),
                    function: function_name.to_string(),
                    message: "Add 'api_extensions = true' to [permissions] in plugin.toml".to_string(),
                })
            }
        }

        Permission::Clipboard => {
            if permissions.clipboard {
                PermissionCheck::Allowed
            } else {
                PermissionCheck::Denied(PermissionDenied {
                    required: "clipboard".to_string(),
                    function: function_name.to_string(),
                    message: "Add 'clipboard = true' to [permissions] in plugin.toml".to_string(),
                })
            }
        }

        Permission::RunSubprocess => {
            if permissions.run_subprocess {
                PermissionCheck::Allowed
            } else {
                PermissionCheck::Denied(PermissionDenied {
                    required: "run_subprocess".to_string(),
                    function: function_name.to_string(),
                    message: "Add 'run_subprocess = true' to [permissions] in plugin.toml. This is a super-permission: the subprocess can access the full filesystem.".to_string(),
                })
            }
        }

        Permission::FsRead(path) => {
            if check_path_permission(&permissions.fs_read, path) {
                PermissionCheck::Allowed
            } else {
                PermissionCheck::Denied(PermissionDenied {
                    required: "fs_read".to_string(),
                    function: function_name.to_string(),
                    message: format!(
                        "Path '{}' is not covered by fs_read permissions. Add a matching glob to fs_read in plugin.toml.",
                        path
                    ),
                })
            }
        }

        Permission::FsWrite(path) => {
            if check_path_permission(&permissions.fs_write, path) {
                PermissionCheck::Allowed
            } else {
                PermissionCheck::Denied(PermissionDenied {
                    required: "fs_write".to_string(),
                    function: function_name.to_string(),
                    message: format!(
                        "Path '{}' is not covered by fs_write permissions. Add a matching glob to fs_write in plugin.toml.",
                        path
                    ),
                })
            }
        }

        Permission::Network => {
            if permissions.network {
                PermissionCheck::Allowed
            } else {
                PermissionCheck::Denied(PermissionDenied {
                    required: "network".to_string(),
                    function: function_name.to_string(),
                    message: "Add 'network = true' to [permissions] in plugin.toml".to_string(),
                })
            }
        }
    }
}

/// Check if a path matches any of the allowed path globs.
///
/// Path globs use basic glob syntax (e.g., "/abs/path/**/*.git/**").
/// The path is canonicalized before matching to prevent traversal attacks.
fn check_path_permission(allowed_globs: &[String], path: &str) -> bool {
    if allowed_globs.is_empty() {
        return false;
    }

    // Canonicalize the requested path to prevent traversal
    let canonical = match std::fs::canonicalize(path) {
        Ok(p) => p,
        // If the path does not exist, try to canonicalize the parent
        Err(_) => {
            let p = std::path::Path::new(path);
            match p.parent().and_then(|parent| std::fs::canonicalize(parent).ok()) {
                Some(parent) => parent.join(p.file_name().unwrap_or_default()),
                None => return false,
            }
        }
    };
    let canonical_str = canonical.to_string_lossy();

    for glob_pattern in allowed_globs {
        if glob_matches(glob_pattern, &canonical_str) {
            return true;
        }
    }

    false
}

/// Simple glob matching for filesystem paths.
/// Supports `*` (single segment) and `**` (multiple segments).
fn glob_matches(pattern: &str, path: &str) -> bool {
    // Use the glob crate for robust matching, or implement a simple version
    // For now, use a straightforward approach:
    if pattern.contains("**") {
        // "**" matches any number of path segments
        let parts: Vec<&str> = pattern.split("**").collect();
        if parts.len() == 2 {
            let prefix = parts[0].trim_end_matches('/');
            let suffix = parts[1].trim_start_matches('/');
            if !path.starts_with(prefix) {
                return false;
            }
            if suffix.is_empty() {
                return true;
            }
            return path.ends_with(suffix);
        }
    }

    // Simple wildcard matching
    pattern == path
}

/// Map a host function name to the required permission.
///
/// This is the central permission table. Every host function must
/// be listed here. Functions not in this table default to Permission::None.
pub fn required_permission(function_name: &str) -> Permission {
    match function_name {
        // Tier 1: Read (no permissions)
        "get-active-pane" | "get-pane" | "list-panes"
        | "get-active-window" | "get-window" | "list-windows"
        | "get-active-session" | "get-session" | "list-sessions"
        | "get-config" => Permission::None,

        // Tier 2: Display (no permissions)
        "set-status-segment" | "set-badge" | "clear-badge"
        | "emit-event" | "show-overlay" | "hide-overlay"
        | "log" => Permission::None,

        // Tier 3: Control
        "create-pane" | "split-pane" | "create-floating-pane"
        | "close-pane" | "toggle-floating-pane" | "focus-pane"
        | "resize-pane" | "rename-pane" | "set-pane-tag"
        | "clear-pane-tag" | "register-layout" | "apply-layout" => Permission::ManagePanes,

        "send-keys" | "send-text" => Permission::SendKeys,

        "read-pane-output" | "read-pane-scrollback" => Permission::ReadPaneOutput,

        "create-session" | "create-window" | "close-window"
        | "kill-session" | "rename-session" | "rename-window"
        | "focus-window" => Permission::ManageSessions,

        // Tier 4: Extend
        "register-api-method" | "register-command-override" => Permission::ApiExtensions,
        "get-clipboard" | "set-clipboard" => Permission::Clipboard,
        "run-subprocess" => Permission::RunSubprocess,

        // fs_read / fs_write are checked with path argument at call site
        "read-file" => Permission::FsRead(String::new()), // placeholder; actual path checked at call site
        "write-file" => Permission::FsWrite(String::new()),

        _ => Permission::None,
    }
}
```

### Step 2: Implement the ResourceLimiter

```rust
// crates/shux-plugin/src/sandbox.rs

use wasmtime::{ResourceLimiter, StoreLimits, StoreLimitsBuilder};

/// Default memory limit per plugin (16 MB).
pub const DEFAULT_MEMORY_LIMIT_BYTES: usize = 16 * 1024 * 1024;

/// Default table element limit per plugin.
pub const DEFAULT_TABLE_ELEMENTS: u32 = 10_000;

/// Create StoreLimits for a plugin with the given memory cap.
///
/// The ResourceLimiter is set on the wasmtime Store to enforce
/// per-plugin memory limits. When a plugin tries to grow memory
/// beyond the limit, the grow operation returns an error (trapping
/// the Wasm execution).
pub fn create_store_limits(memory_limit_bytes: usize) -> StoreLimits {
    StoreLimitsBuilder::new()
        .memory_size(memory_limit_bytes)
        .table_elements(DEFAULT_TABLE_ELEMENTS)
        .instances(10)        // Max component instances
        .memories(10)         // Max memory instances
        .tables(10)           // Max table instances
        .build()
}

/// Configure WASI capabilities for a plugin based on its permissions.
///
/// PRD section 13: WASI filesystem sandbox; only paths listed in
/// fs_read/fs_write are accessible.
pub struct WasiSandboxConfig {
    /// Directories accessible for reading.
    pub readable_dirs: Vec<String>,
    /// Directories accessible for writing.
    pub writable_dirs: Vec<String>,
    /// Whether network access is allowed.
    pub network_enabled: bool,
    /// Environment variables visible to the plugin.
    pub env_vars: Vec<(String, String)>,
}

impl WasiSandboxConfig {
    /// Create a WASI sandbox config from plugin permissions.
    pub fn from_permissions(
        permissions: &crate::manifest::PluginPermissions,
    ) -> Self {
        Self {
            readable_dirs: permissions.fs_read.clone(),
            writable_dirs: permissions.fs_write.clone(),
            network_enabled: permissions.network,
            env_vars: Vec::new(), // Plugins get no env vars by default
        }
    }

    /// Create a minimal sandbox with no filesystem or network access.
    pub fn minimal() -> Self {
        Self {
            readable_dirs: Vec::new(),
            writable_dirs: Vec::new(),
            network_enabled: false,
            env_vars: Vec::new(),
        }
    }
}
```

### Step 3: Implement sandboxed subprocess execution

```rust
// crates/shux-plugin/src/subprocess.rs

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

/// Default subprocess timeout (30 seconds).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Environment variables allowed in subprocess execution.
/// All other env vars are scrubbed.
const ALLOWED_ENV_VARS: &[&str] = &["PATH", "HOME", "TERM", "LANG"];

/// Execute a subprocess with sandbox restrictions.
///
/// PRD section 13 (Security) and section 7.4 (exec permission):
/// - Scrubbed environment (only PATH, HOME, TERM, LANG)
/// - CWD restriction
/// - 30s timeout (configurable)
/// - Direct invocation (no shell) to prevent injection
/// - On Linux: rlimits (future enhancement)
pub async fn run_sandboxed(
    command: &str,
    args: &[String],
    cwd: Option<&str>,
    additional_env: &[(String, String)],
    timeout: Option<Duration>,
) -> Result<SubprocessResult, SubprocessError> {
    let timeout = timeout.unwrap_or(DEFAULT_TIMEOUT);

    // Build command with scrubbed environment
    let mut cmd = Command::new(command);
    cmd.args(args);

    // Clear all environment variables, then add only allowed ones
    cmd.env_clear();
    for var_name in ALLOWED_ENV_VARS {
        if let Ok(value) = std::env::var(var_name) {
            cmd.env(var_name, value);
        }
    }

    // Add plugin-declared additional env vars
    for (key, value) in additional_env {
        cmd.env(key, value);
    }

    // Set working directory
    if let Some(dir) = cwd {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            cmd.current_dir(path);
        } else {
            return Err(SubprocessError::InvalidCwd(dir.to_string()));
        }
    }

    // Capture stdout/stderr, provide no stdin
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.stdin(Stdio::null());

    // Spawn the process
    let mut child = cmd.spawn().map_err(|e| SubprocessError::SpawnFailed {
        command: command.to_string(),
        error: e.to_string(),
    })?;

    // Wait with timeout
    let result = tokio::time::timeout(timeout, child.wait_with_output()).await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(-1);

            Ok(SubprocessResult {
                stdout,
                stderr,
                exit_code,
            })
        }
        Ok(Err(e)) => Err(SubprocessError::WaitFailed(e.to_string())),
        Err(_) => {
            // Timeout: kill the process
            let _ = child.kill().await;
            Err(SubprocessError::Timeout {
                command: command.to_string(),
                timeout_secs: timeout.as_secs(),
            })
        }
    }
}

/// Result of a sandboxed subprocess execution.
#[derive(Debug, Clone)]
pub struct SubprocessResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum SubprocessError {
    #[error("failed to spawn '{command}': {error}")]
    SpawnFailed { command: String, error: String },
    #[error("failed to wait for process: {0}")]
    WaitFailed(String),
    #[error("subprocess '{command}' timed out after {timeout_secs}s")]
    Timeout { command: String, timeout_secs: u64 },
    #[error("invalid working directory: {0}")]
    InvalidCwd(String),
}
```

### Step 4: Integrate ResourceLimiter into Store creation

Modify `crates/shux-plugin/src/wasm.rs` to set up the `StoreLimits` on each plugin's Store:

```rust
// In WasmPlugin::load(), after creating the Store:

use crate::sandbox::{create_store_limits, DEFAULT_MEMORY_LIMIT_BYTES};

// Create the per-plugin Store with resource limits
let state = PluginState {
    id: manifest.plugin.id.clone(),
    manifest: manifest.clone(),
};
let mut store = Store::new(engine, state);

// Set memory limits via StoreLimits
let limits = create_store_limits(DEFAULT_MEMORY_LIMIT_BYTES);
store.limiter(|_| &limits);

// Configure epoch deadline for CPU timeout
store.set_epoch_deadline(epoch_deadline_ticks());
```

### Step 5: Add permission checks to host function stubs

In `crates/shux-plugin/src/host.rs`, wrap each host function with a permission check:

```rust
// crates/shux-plugin/src/host.rs

use crate::permissions::{check_permission, required_permission, PermissionCheck};
use crate::wasm::PluginState;

/// Macro to generate a permission-checked host function.
///
/// Before executing the actual logic, checks the plugin's permissions.
/// If denied, returns a host-error with code -1 and a clear message.
macro_rules! permission_checked {
    ($store:expr, $func_name:literal, $body:expr) => {{
        let state = $store.data();
        let perm = required_permission($func_name);
        match check_permission(&state.manifest.permissions, $func_name, &perm) {
            PermissionCheck::Allowed => $body,
            PermissionCheck::Denied(denied) => {
                tracing::warn!(
                    plugin = %state.id,
                    function = $func_name,
                    permission = %denied.required,
                    "Permission denied"
                );
                Err(HostError {
                    code: -1,
                    message: denied.to_string(),
                })
            }
        }
    }};
}

// Example host function with permission check:
//
// fn host_create_pane(
//     store: &mut Store<PluginState>,
//     options: PaneCreateOptions,
// ) -> Result<String, HostError> {
//     permission_checked!(store, "create-pane", {
//         // Actual pane creation logic (task 040)
//         todo!()
//     })
// }
```

### Step 6: Write comprehensive tests

```rust
#[cfg(test)]
mod permission_tests {
    use crate::manifest::PluginPermissions;
    use crate::permissions::*;

    fn perms_with(setter: impl FnOnce(&mut PluginPermissions)) -> PluginPermissions {
        let mut p = PluginPermissions::default();
        setter(&mut p);
        p
    }

    #[test]
    fn test_query_functions_need_no_permission() {
        let perms = PluginPermissions::default();
        let functions = [
            "get-active-pane", "get-pane", "list-panes",
            "get-active-window", "list-windows", "get-config",
        ];
        for func in &functions {
            let perm = required_permission(func);
            assert_eq!(perm, Permission::None, "Expected no permission for {}", func);
            let result = check_permission(&perms, func, &perm);
            assert!(matches!(result, PermissionCheck::Allowed));
        }
    }

    #[test]
    fn test_display_functions_need_no_permission() {
        let perms = PluginPermissions::default();
        let functions = ["set-status-segment", "set-badge", "emit-event", "log"];
        for func in &functions {
            let result = check_permission(&perms, func, &required_permission(func));
            assert!(matches!(result, PermissionCheck::Allowed));
        }
    }

    #[test]
    fn test_manage_panes_required_for_create_pane() {
        let denied_perms = PluginPermissions::default();
        let result = check_permission(&denied_perms, "create-pane", &Permission::ManagePanes);
        assert!(matches!(result, PermissionCheck::Denied(_)));

        let allowed_perms = perms_with(|p| p.manage_panes = true);
        let result = check_permission(&allowed_perms, "create-pane", &Permission::ManagePanes);
        assert!(matches!(result, PermissionCheck::Allowed));
    }

    #[test]
    fn test_send_keys_denied_by_default() {
        let perms = PluginPermissions::default();
        let result = check_permission(&perms, "send-keys", &Permission::SendKeys);
        assert!(matches!(result, PermissionCheck::Denied(_)));
    }

    #[test]
    fn test_send_keys_allowed_when_granted() {
        let perms = perms_with(|p| p.send_keys = true);
        let result = check_permission(&perms, "send-keys", &Permission::SendKeys);
        assert!(matches!(result, PermissionCheck::Allowed));
    }

    #[test]
    fn test_subprocess_denied_by_default() {
        let perms = PluginPermissions::default();
        let result = check_permission(&perms, "run-subprocess", &Permission::RunSubprocess);
        assert!(matches!(result, PermissionCheck::Denied(_)));
    }

    #[test]
    fn test_clipboard_denied_by_default() {
        let perms = PluginPermissions::default();
        let result = check_permission(&perms, "get-clipboard", &Permission::Clipboard);
        assert!(matches!(result, PermissionCheck::Denied(_)));
    }

    #[test]
    fn test_denied_error_message_is_actionable() {
        let perms = PluginPermissions::default();
        let result = check_permission(&perms, "send-keys", &Permission::SendKeys);
        if let PermissionCheck::Denied(denied) = result {
            assert!(denied.message.contains("plugin.toml"));
            assert!(denied.message.contains("send_keys"));
            assert!(denied.required == "send_keys");
        } else {
            panic!("Expected Denied");
        }
    }

    #[test]
    fn test_all_tier3_functions_require_permissions() {
        let perms = PluginPermissions::default(); // all denied
        let tier3_funcs = [
            "create-pane", "split-pane", "close-pane", "focus-pane",
            "send-keys", "send-text", "read-pane-output",
            "create-session", "kill-session",
        ];
        for func in &tier3_funcs {
            let perm = required_permission(func);
            assert_ne!(perm, Permission::None, "{} should require a permission", func);
        }
    }
}

#[cfg(test)]
mod sandbox_tests {
    use crate::sandbox::*;

    #[test]
    fn test_store_limits_creation() {
        let limits = create_store_limits(DEFAULT_MEMORY_LIMIT_BYTES);
        // StoreLimits is opaque; we just verify it does not panic
        let _ = limits;
    }

    #[test]
    fn test_wasi_sandbox_from_permissions() {
        let mut perms = crate::manifest::PluginPermissions::default();
        perms.fs_read = vec!["/home/user/.config/**".to_string()];
        perms.network = true;

        let sandbox = WasiSandboxConfig::from_permissions(&perms);
        assert_eq!(sandbox.readable_dirs.len(), 1);
        assert!(sandbox.writable_dirs.is_empty());
        assert!(sandbox.network_enabled);
    }

    #[test]
    fn test_minimal_sandbox() {
        let sandbox = WasiSandboxConfig::minimal();
        assert!(sandbox.readable_dirs.is_empty());
        assert!(sandbox.writable_dirs.is_empty());
        assert!(!sandbox.network_enabled);
        assert!(sandbox.env_vars.is_empty());
    }
}

#[cfg(test)]
mod subprocess_tests {
    use crate::subprocess::*;

    #[tokio::test]
    async fn test_run_echo() {
        let result = run_sandboxed(
            "echo",
            &["hello".to_string()],
            None,
            &[],
            None,
        ).await.unwrap();

        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.trim() == "hello");
    }

    #[tokio::test]
    async fn test_run_with_timeout() {
        let result = run_sandboxed(
            "sleep",
            &["60".to_string()],
            None,
            &[],
            Some(std::time::Duration::from_millis(100)),
        ).await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), SubprocessError::Timeout { .. }));
    }

    #[tokio::test]
    async fn test_run_nonexistent_command() {
        let result = run_sandboxed(
            "nonexistent_command_xyz",
            &[],
            None,
            &[],
            None,
        ).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_env_is_scrubbed() {
        // Set a custom env var that should NOT be visible
        std::env::set_var("SHUX_TEST_SECRET", "sensitive_data");

        let result = run_sandboxed(
            "env",
            &[],
            None,
            &[],
            None,
        ).await.unwrap();

        assert!(!result.stdout.contains("SHUX_TEST_SECRET"));
        // PATH should still be present
        assert!(result.stdout.contains("PATH="));

        std::env::remove_var("SHUX_TEST_SECRET");
    }
}
```

---

## Verification

### Functional

```bash
# Build the plugin crate
cargo build -p shux-plugin

# Verify clippy
cargo clippy -p shux-plugin -- -D warnings

# Verify that a default-permissions plugin cannot call send-keys
# (integration test with actual Wasm plugin, or unit test)
```

### Tests

```bash
# Run all plugin tests
cargo nextest run -p shux-plugin

# Permission tests
cargo nextest run -p shux-plugin -- permission

# Sandbox tests
cargo nextest run -p shux-plugin -- sandbox

# Subprocess tests
cargo nextest run -p shux-plugin -- subprocess

# Full workspace
cargo nextest run --workspace
```

---

## Completion Criteria

- [ ] `Permission` enum covers all host function permission requirements
- [ ] `required_permission()` maps every WIT host function to its required permission
- [ ] `check_permission()` consults `PluginPermissions` and returns `Allowed` or `Denied`
- [ ] Permission-denied errors include: required permission name, function name, actionable hint
- [ ] `ResourceLimiter` via `StoreLimits` set on every plugin Store (default 16 MB memory cap)
- [ ] Epoch interruption configured (100ms kill threshold, ~10% overhead)
- [ ] `WasiSandboxConfig` configures WASI capabilities from `plugin.toml` permissions
- [ ] Only `fs_read`/`fs_write` glob-matched paths are accessible
- [ ] `run_sandboxed()` runs subprocess with scrubbed environment (PATH, HOME, TERM, LANG only)
- [ ] Subprocess has 30s timeout, direct invocation (no shell), CWD restriction
- [ ] Network disabled by default, enabled only with `network = true` permission
- [ ] `permission_checked!` macro gates host function calls with permission checks
- [ ] All Tier 1/2 functions (queries, display) require no permissions
- [ ] All Tier 3/4 functions (control, extend) require specific permissions
- [ ] All unit tests pass (permission checks, sandbox config, subprocess execution)
- [ ] `cargo clippy -p shux-plugin -- -D warnings` passes

---

## Commit Message
```
feat(plugin): add permission enforcement and sandbox for Wasm plugins

- Permission checker: maps every host function to required permission
- Four-tier graduated permissions (Read, Display, Control, Extend)
- ResourceLimiter via StoreLimits (16 MB default memory cap)
- Epoch interruption for CPU timeout (100ms kill threshold)
- WASI filesystem sandbox: only declared fs_read/fs_write paths
- Sandboxed subprocess: scrubbed env, no shell, 30s timeout, CWD restriction
- Actionable permission-denied error messages for plugin developers
```

---

## Session Protocol

1. **Before starting:** Read `CLAUDE.md`. Read task 038 (wasmtime integration) to understand the Engine/Store/Linker architecture. Read PRD sections 7.1 (safe by default), 7.4 (permissions), 7.5 (graduated control plane), and 13 (security). Verify task 038 is complete.
2. **During:** Implement in order: permission checker (Step 1) -> ResourceLimiter (Step 2) -> subprocess sandbox (Step 3) -> Store integration (Step 4) -> host function gating (Step 5) -> tests (Step 6). Run `cargo check -p shux-plugin` after each step. The permission checker and subprocess sandbox are independent modules that can be tested in isolation.
3. **Testing:** Test every permission tier boundary. A default-permissions plugin should be able to call all Tier 1/2 functions but be denied on all Tier 3/4 functions. The subprocess test should verify environment scrubbing by checking that custom env vars are not visible.
4. **After:** Run `make check`. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings with any discoveries about wasmtime ResourceLimiter behavior, WASI capability API, or subprocess environment handling on macOS vs Linux.
5. **Watch out for:**
   - `StoreLimitsBuilder` API may differ between wasmtime versions. Check the wasmtime 41 docs.
   - Filesystem path canonicalization can fail for non-existent paths. The glob matcher must handle this gracefully.
   - On macOS, `env` command path may differ from Linux. Use `/usr/bin/env` or handle the difference.
   - The subprocess timeout test (`sleep 60` with 100ms timeout) must actually kill the process. Verify with `tokio::process::Child::kill()`.
   - The `permission_checked!` macro should log denied attempts at WARN level for debugging.
