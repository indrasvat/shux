# 054 — Shell Completions (bash, zsh, fish)

**Status:** Pending
**Depends On:** 052
**Parallelizable With:** 053, 055, 056, 057

---

## Problem

A polished CLI tool needs shell completions. Without them, users constantly reach for `shux --help` and `shux <subcommand> --help` to remember flag names and available subcommands. The PRD requires bash, zsh, and fish completions as part of M3 polish. clap provides a `generate` feature for static completions, but shux also needs dynamic completions that query the running daemon for session names, window names, pane IDs, theme names, and plugin IDs. This makes tab-completion context-aware: `shux attach -s <TAB>` lists actual session names, not just placeholder text.

## PRD Reference

- **SS 17** M3 deliverables: "Shell completions (bash, zsh, fish)"
- **SS 15.2** Key crates: "`clap` (derive) 4.x — Subcommands, completions"
- **SS 8.6** CLI-API mapping: Complete list of `shux` subcommands and flags

---

## Files to Create

- `crates/shux/src/completions.rs` — Completion generation module
- `crates/shux/src/completions/dynamic.rs` — Dynamic completions (queries daemon)
- `scripts/completions/shux.bash` — Generated bash completion (committed for reference)
- `scripts/completions/shux.zsh` — Generated zsh completion (committed for reference)
- `scripts/completions/shux.fish` — Generated fish completion (committed for reference)

## Files to Modify

- `crates/shux/src/main.rs` — Add `completions` subcommand
- `crates/shux/src/cli.rs` — Register completions subcommand in clap
- `crates/shux/Cargo.toml` — Enable `clap_complete` dependency
- `docs/PROGRESS.md` — Mark task 054 complete

---

## Execution Steps

### Step 1: Add clap_complete Dependency

Update `crates/shux/Cargo.toml`:

```toml
[dependencies]
clap_complete = "4"
clap_complete_nushell = "4"  # bonus: nushell completions
```

### Step 2: Create Completions Subcommand

Add to the CLI definition in `crates/shux/src/cli.rs`:

```rust
#[derive(Subcommand)]
pub enum Commands {
    // ... existing subcommands ...

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: CompletionShell,
    },
}

#[derive(Clone, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
    Elvish,
    Nushell,
}
```

### Step 3: Implement Static Completion Generation

Create `crates/shux/src/completions.rs`:

```rust
//! Shell completion generation for shux.
//!
//! Supports two modes:
//! 1. Static: `shux completions bash` — generates a completion script
//!    for installation. Uses clap_complete for subcommands and flags.
//! 2. Dynamic: Runtime completion that queries the daemon for session
//!    names, window names, pane IDs, themes, and plugin IDs.
//!
//! Installation:
//!   bash: shux completions bash > ~/.local/share/bash-completion/completions/shux
//!   zsh:  shux completions zsh > "${fpath[1]}/_shux"
//!   fish: shux completions fish > ~/.config/fish/completions/shux.fish

mod dynamic;

use clap::Command;
use clap_complete::{generate, Shell as ClapShell};
use std::io;

/// Generate a static completion script and write it to stdout.
pub fn generate_completions(shell: &super::cli::CompletionShell, cmd: &mut Command) {
    let clap_shell = match shell {
        super::cli::CompletionShell::Bash => ClapShell::Bash,
        super::cli::CompletionShell::Zsh => ClapShell::Zsh,
        super::cli::CompletionShell::Fish => ClapShell::Fish,
        super::cli::CompletionShell::PowerShell => ClapShell::PowerShell,
        super::cli::CompletionShell::Elvish => ClapShell::Elvish,
        super::cli::CompletionShell::Nushell => {
            generate_nushell_completions(cmd);
            return;
        }
    };

    generate(clap_shell, cmd, "shux", &mut io::stdout());
}

fn generate_nushell_completions(cmd: &mut Command) {
    use clap_complete_nushell::Nushell;
    generate(Nushell, cmd, "shux", &mut io::stdout());
}
```

### Step 4: Implement Dynamic Completions

Create `crates/shux/src/completions/dynamic.rs`:

```rust
//! Dynamic completions that query the running daemon.
//!
//! When the user presses TAB after certain flags, the shell calls back
//! into shux to get context-aware completions. For example:
//!
//! - `shux attach -s <TAB>` → lists session names
//! - `shux kill -s <TAB>` → lists session names
//! - `shux theme set <TAB>` → lists theme names
//! - `shux plugin reload <TAB>` → lists plugin IDs
//!
//! Dynamic completion works by:
//! 1. The generated completion script calls `shux --complete <context>`
//! 2. shux connects to the daemon, queries the relevant list, and prints
//!    one completion per line
//! 3. If the daemon is not running, falls back to empty completions
//!    (never errors — broken completions are worse than no completions)

use std::io::{self, Write};

/// Provide dynamic completions for the given context.
///
/// context format: "<subcommand> <flag>" e.g., "attach -s" or "theme set"
pub fn complete(context: &str) -> io::Result<()> {
    let parts: Vec<&str> = context.split_whitespace().collect();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let completions = match parts.as_slice() {
        // Session name completions
        ["attach", "-s"] | ["kill", "-s"] | ["rename", "-s"] => {
            query_daemon_sessions()
        }

        // Window completions
        ["window", "focus"] | ["window", "rename"] | ["window", "kill"] => {
            query_daemon_windows()
        }

        // Pane completions
        ["pane", "focus"] | ["pane", "resize"] | ["pane", "kill"]
        | ["split", "-p"] | ["capture", "-p"] | ["run", "-p"] => {
            query_daemon_panes()
        }

        // Theme completions
        ["theme", "set"] | ["theme", "get"] => {
            query_daemon_themes()
        }

        // Plugin completions
        ["plugin", "enable"] | ["plugin", "disable"]
        | ["plugin", "reload"] | ["plugin", "inspect"] => {
            query_daemon_plugins()
        }

        // Template completions
        ["apply"] => {
            list_template_files()
        }

        _ => Vec::new(),
    };

    for completion in completions {
        writeln!(out, "{}", completion)?;
    }

    Ok(())
}

/// Query the daemon for session names. Returns empty Vec on failure.
fn query_daemon_sessions() -> Vec<String> {
    match connect_and_call("session.list", serde_json::json!({})) {
        Ok(resp) => resp["result"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn query_daemon_windows() -> Vec<String> {
    match connect_and_call("window.list", serde_json::json!({})) {
        Ok(resp) => resp["result"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|w| w["title"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn query_daemon_panes() -> Vec<String> {
    match connect_and_call("pane.list", serde_json::json!({})) {
        Ok(resp) => resp["result"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| {
                        let id = p["id"].as_str()?;
                        let title = p["title"].as_str().unwrap_or("");
                        if title.is_empty() {
                            Some(id.to_string())
                        } else {
                            Some(format!("{}\t{}", id, title))
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn query_daemon_themes() -> Vec<String> {
    match connect_and_call("theme.list", serde_json::json!({})) {
        Ok(resp) => resp["result"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn query_daemon_plugins() -> Vec<String> {
    match connect_and_call("plugin.list", serde_json::json!({})) {
        Ok(resp) => resp["result"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| p["id"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn list_template_files() -> Vec<String> {
    let config_dir = dirs::config_dir()
        .map(|d| d.join("shux/templates"))
        .unwrap_or_default();

    let mut templates = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&config_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "toml") {
                if let Some(name) = path.file_stem().and_then(|n| n.to_str()) {
                    templates.push(name.to_string());
                }
            }
        }
    }

    // Also complete file paths in current directory
    if let Ok(entries) = std::fs::read_dir(".") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "toml") {
                if let Some(name) = path.to_str() {
                    templates.push(name.to_string());
                }
            }
        }
    }

    templates
}

/// Connect to the daemon and make a JSON-RPC call.
/// Returns Err if daemon is not running — this is expected and must not
/// produce any error output (it would corrupt completion results).
fn connect_and_call(
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let socket_path = get_socket_path();
    let mut stream = UnixStream::connect(&socket_path)?;
    stream.set_read_timeout(Some(std::time::Duration::from_millis(500)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_millis(500)))?;

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "completion",
        "method": method,
        "params": params,
    });

    let payload = serde_json::to_vec(&request)?;
    let len_bytes = (payload.len() as u32).to_be_bytes();
    stream.write_all(&len_bytes)?;
    stream.write_all(&payload)?;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let resp_len = u32::from_be_bytes(len_buf) as usize;

    let mut resp_buf = vec![0u8; resp_len];
    stream.read_exact(&mut resp_buf)?;

    Ok(serde_json::from_slice(&resp_buf)?)
}

fn get_socket_path() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("SHUX_SOCKET") {
        return std::path::PathBuf::from(path);
    }

    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/tmp/shux-{}", users::get_current_uid()));

    std::path::PathBuf::from(runtime_dir).join("shux/shux.sock")
}
```

### Step 5: Wire Completions Subcommand

In `crates/shux/src/main.rs`:

```rust
Commands::Completions { shell } => {
    let mut cmd = Cli::command();
    completions::generate_completions(&shell, &mut cmd);
}
```

### Step 6: Generate and Commit Reference Scripts

```bash
# Generate reference completion scripts
cargo run -p shux -- completions bash > scripts/completions/shux.bash
cargo run -p shux -- completions zsh > scripts/completions/shux.zsh
cargo run -p shux -- completions fish > scripts/completions/shux.fish
```

### Step 7: Add Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completions_generate_without_panic() {
        let mut cmd = crate::cli::Cli::command();
        let shells = [
            CompletionShell::Bash,
            CompletionShell::Zsh,
            CompletionShell::Fish,
        ];
        for shell in &shells {
            // Redirect output to a buffer
            let mut buf = Vec::new();
            // Just verify it doesn't panic
            generate_completions_to_buffer(shell, &mut cmd, &mut buf);
            assert!(!buf.is_empty(), "Completions for {:?} should not be empty", shell);
        }
    }

    #[test]
    fn dynamic_completion_handles_offline_daemon() {
        // When daemon is not running, dynamic completions should return empty
        // and never produce stderr output
        let sessions = dynamic::query_daemon_sessions();
        // No assertion on content — daemon may or may not be running
        // Just verify it doesn't panic
        let _ = sessions;
    }

    #[test]
    fn list_template_files_handles_missing_dir() {
        // Should return empty vec, not error
        let templates = dynamic::list_template_files();
        let _ = templates; // No panic
    }

    #[test]
    fn bash_completion_contains_subcommands() {
        let mut cmd = crate::cli::Cli::command();
        let mut buf = Vec::new();
        clap_complete::generate(ClapShell::Bash, &mut cmd, "shux", &mut buf);
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("attach"));
        assert!(output.contains("split"));
        assert!(output.contains("theme"));
        assert!(output.contains("plugin"));
        assert!(output.contains("completions"));
    }

    #[test]
    fn zsh_completion_contains_descriptions() {
        let mut cmd = crate::cli::Cli::command();
        let mut buf = Vec::new();
        clap_complete::generate(ClapShell::Zsh, &mut cmd, "shux", &mut buf);
        let output = String::from_utf8(buf).unwrap();
        // Zsh completions should include descriptions
        assert!(output.contains("shux"));
    }
}
```

---

## Verification

### Functional

```bash
# Generate completions
shux completions bash > /tmp/shux.bash
shux completions zsh > /tmp/_shux
shux completions fish > /tmp/shux.fish

# Verify bash completions work
source /tmp/shux.bash
shux <TAB><TAB>
# Expected: list of subcommands (new, attach, ls, split, ...)

# Install for testing
shux completions bash > ~/.local/share/bash-completion/completions/shux
# Or for zsh:
shux completions zsh > "${fpath[1]}/_shux"
# Or for fish:
shux completions fish > ~/.config/fish/completions/shux.fish

# Test dynamic completions (daemon must be running)
shux new -s test-session
shux attach -s <TAB>
# Expected: "test-session" appears in completions
```

### Tests

```bash
# Run completion tests
cargo nextest run -p shux completions

# Expected tests:
# - completions_generate_without_panic
# - dynamic_completion_handles_offline_daemon
# - list_template_files_handles_missing_dir
# - bash_completion_contains_subcommands
# - zsh_completion_contains_descriptions
```

---

## Completion Criteria

- [ ] `shux completions bash` generates valid bash completion script
- [ ] `shux completions zsh` generates valid zsh completion script
- [ ] `shux completions fish` generates valid fish completion script
- [ ] Static completions include all subcommands and flags
- [ ] Dynamic completions query daemon for session names
- [ ] Dynamic completions query daemon for window names
- [ ] Dynamic completions query daemon for pane IDs
- [ ] Dynamic completions query daemon for theme names
- [ ] Dynamic completions query daemon for plugin IDs
- [ ] Dynamic completions gracefully handle offline daemon (empty results, no errors)
- [ ] Dynamic completions timeout within 500ms (never slow down shell)
- [ ] Template file completions work for `shux apply <TAB>`
- [ ] Reference completion scripts committed in `scripts/completions/`
- [ ] Installation instructions documented in output comments
- [ ] Tests pass for all shell types

---

## Commit Message

```
feat(cli): add shell completions for bash, zsh, fish with dynamic completions

- Static completions via clap_complete for all subcommands and flags
- Dynamic completions query running daemon for session/window/pane/
  theme/plugin names, with 500ms timeout and graceful offline fallback
- Template file completions for `shux apply`
- Install via: `shux completions bash > ~/.local/share/bash-completion/completions/shux`
- Reference scripts committed in scripts/completions/
```

---

## Session Protocol

1. **Before starting:** Read the clap_complete documentation for the `generate` function. Read SS8.6 for the complete CLI-API mapping to ensure all subcommands have completions. Verify clap 4 + clap_complete 4 version compatibility.
2. **During:** Start with static completions (Steps 1-3), verify they work in a real shell, then add dynamic completions (Step 4). Test dynamic completions with a running daemon. Test offline behavior.
3. **Critical requirement:** Dynamic completions must NEVER produce stderr output. Any error during daemon connection must be silently swallowed. Broken completions are worse than no completions.
4. **Edge cases to watch for:**
   - Daemon not running (most common case — must return empty, not error)
   - Daemon slow to respond (500ms timeout prevents shell hang)
   - Session/window names with spaces or special characters
   - Very long pane IDs (UUIDs)
   - Fish completion format is different from bash/zsh
5. **After:** Test completions in actual bash, zsh, and fish sessions. Verify dynamic completions work with a running daemon. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings (create from task 000 template if missing).
