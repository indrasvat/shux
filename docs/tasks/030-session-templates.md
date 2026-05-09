# 030 — Session Templates

**Status:** Pending
**Depends On:** 022, 015
**Parallelizable With:** 031

---

## Problem

Power users and teams need reproducible workspace setups. A web developer might always want three windows: "editor" (single pane, nvim), "servers" (vertical split, frontend + backend), and "logs" (tail -f). Today they either set this up manually every time or write fragile shell scripts with `shux split` and `shux send-keys` commands with sleep delays.

Session templates solve this with declarative TOML files that describe the complete workspace: session name, windows, pane layout per window, commands to run, themes, and CWDs. `shux apply <template.toml>` creates the entire workspace atomically. Templates support Mustache-style `{{var}}` substitution for portability (e.g., `{{project_dir}}` resolved from CLI flags, environment variables, or built-in defaults).

This maps directly to the `state.apply` API method — creating a session, its windows, and their panes as a single atomic transaction.

## PRD Reference

- **section 6.1** P0 feature matrix, Core multiplexer: session templates (`shux apply <template>`) with layout + commands + themes
- **section 8.2** API methods: `state.apply` — atomic multi-resource creation
- **section 8.5** Agent-safe patterns: use `state.apply` for multi-step atomic changes
- **section 10.3** Session template files: format, variables, layout values

---

## Files to Create

- `crates/shux-core/src/template.rs` — Template parsing, variable resolution, validation
- `crates/shux/src/commands/apply.rs` — CLI `shux apply` command handler

## Files to Modify

- `crates/shux-core/src/lib.rs` — Add `pub mod template;`
- `crates/shux-core/Cargo.toml` — Add `regex` dependency for template variable parsing
- `crates/shux/src/commands/mod.rs` — Register `apply` subcommand
- `crates/shux/src/main.rs` — Wire apply command into CLI

---

## Execution Steps

### Step 1: Define Template Data Model

Create `crates/shux-core/src/template.rs`:

```rust
//! Session template parsing and application.
//!
//! Templates are TOML files that declaratively describe a complete workspace:
//! session name, windows, panes per window, commands, themes, and CWDs.
//!
//! Template variables use Mustache-style `{{var}}` syntax (no logic, no loops).
//! Variables are resolved from:
//! 1. CLI `--var key=value` flags (highest priority)
//! 2. Environment variables prefixed with `SHUX_TPL_` (e.g., SHUX_TPL_PROJECT_NAME)
//! 3. Built-in defaults: {{cwd}} = current directory, {{user}} = $USER
//!
//! Missing required variables cause an actionable error listing all unresolved.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Errors that can occur during template processing.
#[derive(Debug, Error)]
pub enum TemplateError {
    #[error("Failed to read template file: {path}: {source}")]
    ReadError {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("Failed to parse template TOML: {0}")]
    ParseError(#[from] toml::de::Error),

    #[error("Unresolved template variables: {}", variables.join(", "))]
    UnresolvedVariables { variables: Vec<String> },

    #[error("Invalid template: {0}")]
    ValidationError(String),

    #[error("Invalid layout value '{layout}' in window '{window}'. Valid layouts: single, vertical, horizontal, even-vertical, even-horizontal, tiled")]
    InvalidLayout { window: String, layout: String },

    #[error("Template application failed: {0}")]
    ApplicationError(String),
}

/// A parsed session template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTemplate {
    /// Template metadata
    pub template: TemplateMetadata,
    /// Session configuration
    pub session: SessionSpec,
    /// Windows to create
    pub windows: Vec<WindowSpec>,
}

/// Template metadata (the [template] section).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateMetadata {
    /// Template name (for display and identification)
    pub name: String,
    /// Description of what this template sets up
    #[serde(default)]
    pub description: String,
    /// Template version (for future compatibility)
    #[serde(default = "default_version")]
    pub version: String,
}

fn default_version() -> String {
    "1".to_string()
}

/// Session specification from the template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSpec {
    /// Session name (may contain {{variables}})
    pub name: String,
    /// Working directory for the session (may contain {{variables}})
    #[serde(default)]
    pub cwd: Option<String>,
    /// Theme to apply to the session
    #[serde(default)]
    pub theme: Option<String>,
}

/// Window specification from the template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowSpec {
    /// Window title
    pub title: String,
    /// Layout algorithm for panes in this window
    #[serde(default = "default_layout")]
    pub layout: String,
    /// Panes in this window
    #[serde(default)]
    pub panes: Vec<PaneSpec>,
    /// Theme override for this window
    #[serde(default)]
    pub theme: Option<String>,
}

fn default_layout() -> String {
    "single".to_string()
}

/// Pane specification from the template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneSpec {
    /// Command to run in this pane (as argv array)
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Command as a string (alternative to array form, parsed by shell)
    #[serde(default)]
    pub shell_command: Option<String>,
    /// Pane title
    #[serde(default)]
    pub title: Option<String>,
    /// Working directory for this pane (may contain {{variables}})
    #[serde(default)]
    pub cwd: Option<String>,
    /// Theme override for this pane
    #[serde(default)]
    pub theme: Option<String>,
    /// Split ratio (0.0 to 1.0, default 0.5)
    #[serde(default = "default_ratio")]
    pub ratio: f64,
}

fn default_ratio() -> f64 {
    0.5
}

/// Valid layout values.
const VALID_LAYOUTS: &[&str] = &[
    "single",
    "vertical",
    "horizontal",
    "even-vertical",
    "even-horizontal",
    "tiled",
];
```

### Step 2: Implement Template Variable Resolution

```rust
/// Regex pattern for matching {{variable}} placeholders.
const VAR_PATTERN: &str = r"\{\{(\w+)\}\}";

/// Variable resolution context.
pub struct VarContext {
    /// Variables from CLI --var flags
    cli_vars: HashMap<String, String>,
    /// Variables from SHUX_TPL_* environment variables
    env_vars: HashMap<String, String>,
    /// Built-in default variables
    builtins: HashMap<String, String>,
}

impl VarContext {
    /// Create a new variable context.
    ///
    /// # Arguments
    /// * `cli_vars` — Variables from `--var key=value` CLI flags
    pub fn new(cli_vars: HashMap<String, String>) -> Self {
        // Collect SHUX_TPL_* environment variables
        let env_vars: HashMap<String, String> = std::env::vars()
            .filter_map(|(key, value)| {
                key.strip_prefix("SHUX_TPL_")
                    .map(|stripped| (stripped.to_lowercase(), value))
            })
            .collect();

        // Built-in defaults
        let mut builtins = HashMap::new();
        builtins.insert(
            "cwd".to_string(),
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
        );
        builtins.insert(
            "user".to_string(),
            std::env::var("USER").unwrap_or_else(|_| "unknown".to_string()),
        );
        builtins.insert(
            "home".to_string(),
            std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()),
        );
        builtins.insert(
            "hostname".to_string(),
            hostname::get()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_default(),
        );

        Self {
            cli_vars,
            env_vars,
            builtins,
        }
    }

    /// Resolve a variable name to its value.
    /// Priority: CLI > env > builtins
    pub fn resolve(&self, name: &str) -> Option<&str> {
        self.cli_vars
            .get(name)
            .or_else(|| self.env_vars.get(name))
            .or_else(|| self.builtins.get(name))
            .map(|s| s.as_str())
    }

    /// Substitute all {{variables}} in a string.
    /// Returns the substituted string and a list of unresolved variables.
    pub fn substitute(&self, input: &str) -> (String, Vec<String>) {
        let re = regex::Regex::new(VAR_PATTERN).expect("valid regex");
        let mut unresolved = Vec::new();
        let result = re.replace_all(input, |caps: &regex::Captures| {
            let var_name = &caps[1];
            match self.resolve(var_name) {
                Some(value) => value.to_string(),
                None => {
                    unresolved.push(var_name.to_string());
                    caps[0].to_string() // Keep the {{var}} placeholder
                }
            }
        });
        (result.to_string(), unresolved)
    }
}

impl SessionTemplate {
    /// Load and parse a template from a TOML file.
    pub fn load(path: &Path) -> Result<Self, TemplateError> {
        let content = std::fs::read_to_string(path).map_err(|e| TemplateError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        let template: SessionTemplate = toml::from_str(&content)?;
        template.validate()?;
        Ok(template)
    }

    /// Parse a template from a TOML string (for testing).
    pub fn parse(content: &str) -> Result<Self, TemplateError> {
        let template: SessionTemplate = toml::from_str(content)?;
        template.validate()?;
        Ok(template)
    }

    /// Validate the template structure.
    fn validate(&self) -> Result<(), TemplateError> {
        if self.template.name.is_empty() {
            return Err(TemplateError::ValidationError(
                "Template name cannot be empty".into(),
            ));
        }

        if self.windows.is_empty() {
            return Err(TemplateError::ValidationError(
                "Template must define at least one window".into(),
            ));
        }

        for window in &self.windows {
            // Validate layout
            if !VALID_LAYOUTS.contains(&window.layout.as_str()) {
                return Err(TemplateError::InvalidLayout {
                    window: window.title.clone(),
                    layout: window.layout.clone(),
                });
            }

            // Validate pane count vs layout
            match window.layout.as_str() {
                "single" if window.panes.len() > 1 => {
                    return Err(TemplateError::ValidationError(format!(
                        "Window '{}' uses 'single' layout but has {} panes",
                        window.title,
                        window.panes.len()
                    )));
                }
                _ => {}
            }

            // Validate pane ratios
            for pane in &window.panes {
                if pane.ratio <= 0.0 || pane.ratio > 1.0 {
                    return Err(TemplateError::ValidationError(format!(
                        "Pane ratio must be between 0.0 and 1.0, got {} in window '{}'",
                        pane.ratio, window.title
                    )));
                }
            }
        }

        Ok(())
    }

    /// Resolve all template variables and return a fully-resolved template.
    pub fn resolve(&self, vars: &VarContext) -> Result<ResolvedTemplate, TemplateError> {
        let mut all_unresolved = Vec::new();

        // Resolve session name
        let (session_name, unresolved) = vars.substitute(&self.session.name);
        all_unresolved.extend(unresolved);

        // Resolve session CWD
        let session_cwd = self.session.cwd.as_ref().map(|cwd| {
            let (resolved, unresolved) = vars.substitute(cwd);
            all_unresolved.extend(unresolved);
            resolved
        });

        // Resolve windows
        let mut resolved_windows = Vec::new();
        for window in &self.windows {
            let (title, unresolved) = vars.substitute(&window.title);
            all_unresolved.extend(unresolved);

            let mut resolved_panes = Vec::new();
            for pane in &window.panes {
                // Resolve command arguments
                let command = pane.command.as_ref().map(|cmd| {
                    cmd.iter()
                        .map(|arg| {
                            let (resolved, unresolved) = vars.substitute(arg);
                            all_unresolved.extend(unresolved);
                            resolved
                        })
                        .collect()
                });

                let shell_command = pane.shell_command.as_ref().map(|cmd| {
                    let (resolved, unresolved) = vars.substitute(cmd);
                    all_unresolved.extend(unresolved);
                    resolved
                });

                let cwd = pane.cwd.as_ref().map(|c| {
                    let (resolved, unresolved) = vars.substitute(c);
                    all_unresolved.extend(unresolved);
                    resolved
                });

                let pane_title = pane.title.as_ref().map(|t| {
                    let (resolved, unresolved) = vars.substitute(t);
                    all_unresolved.extend(unresolved);
                    resolved
                });

                resolved_panes.push(ResolvedPaneSpec {
                    command,
                    shell_command,
                    title: pane_title,
                    cwd,
                    theme: pane.theme.clone(),
                    ratio: pane.ratio,
                });
            }

            resolved_windows.push(ResolvedWindowSpec {
                title,
                layout: window.layout.clone(),
                panes: resolved_panes,
                theme: window.theme.clone(),
            });
        }

        // Check for unresolved variables
        all_unresolved.sort();
        all_unresolved.dedup();
        if !all_unresolved.is_empty() {
            return Err(TemplateError::UnresolvedVariables {
                variables: all_unresolved,
            });
        }

        Ok(ResolvedTemplate {
            name: self.template.name.clone(),
            session_name,
            session_cwd,
            session_theme: self.session.theme.clone(),
            windows: resolved_windows,
        })
    }
}

/// A fully resolved template with all variables substituted.
#[derive(Debug, Clone)]
pub struct ResolvedTemplate {
    pub name: String,
    pub session_name: String,
    pub session_cwd: Option<String>,
    pub session_theme: Option<String>,
    pub windows: Vec<ResolvedWindowSpec>,
}

#[derive(Debug, Clone)]
pub struct ResolvedWindowSpec {
    pub title: String,
    pub layout: String,
    pub panes: Vec<ResolvedPaneSpec>,
    pub theme: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedPaneSpec {
    pub command: Option<Vec<String>>,
    pub shell_command: Option<String>,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub theme: Option<String>,
    pub ratio: f64,
}
```

### Step 3: Implement Template Application (state.apply Transaction)

```rust
impl ResolvedTemplate {
    /// Apply this template to create a full session.
    ///
    /// This is an atomic transaction: either all resources are created,
    /// or the operation rolls back entirely.
    pub async fn apply(
        &self,
        state: &AppState,
    ) -> Result<ApplyResult, TemplateError> {
        // Build the complete transaction
        let mut operations = Vec::new();

        // 1. Create session
        let session_create = StateMutation::CreateSession {
            name: self.session_name.clone(),
            cwd: self.session_cwd.clone(),
            theme: self.session_theme.clone(),
        };
        operations.push(session_create);

        // 2. Create windows and panes
        for (win_idx, window) in self.windows.iter().enumerate() {
            let window_create = StateMutation::CreateWindow {
                session_name: self.session_name.clone(),
                title: window.title.clone(),
                theme: window.theme.clone(),
            };
            operations.push(window_create);

            // Create panes based on layout
            match window.layout.as_str() {
                "single" => {
                    // Single pane — just the first (or default shell)
                    if let Some(pane) = window.panes.first() {
                        operations.push(self.pane_create_op(pane, win_idx));
                    }
                }
                "vertical" | "horizontal" => {
                    // Two or more panes split in one direction
                    let direction = if window.layout == "vertical" {
                        SplitDirection::Vertical
                    } else {
                        SplitDirection::Horizontal
                    };

                    for (i, pane) in window.panes.iter().enumerate() {
                        if i == 0 {
                            // First pane is the initial pane of the window
                            operations.push(self.pane_create_op(pane, win_idx));
                        } else {
                            // Subsequent panes are splits
                            operations.push(StateMutation::SplitPane {
                                direction,
                                ratio: pane.ratio,
                                command: pane.resolved_command(),
                                cwd: pane.cwd.clone(),
                                title: pane.title.clone(),
                                theme: pane.theme.clone(),
                            });
                        }
                    }
                }
                "even-vertical" | "even-horizontal" => {
                    // Equal splits
                    let direction = if window.layout == "even-vertical" {
                        SplitDirection::Vertical
                    } else {
                        SplitDirection::Horizontal
                    };
                    let count = window.panes.len().max(1);
                    let even_ratio = 1.0 / count as f64;

                    for (i, pane) in window.panes.iter().enumerate() {
                        if i == 0 {
                            operations.push(self.pane_create_op(pane, win_idx));
                        } else {
                            operations.push(StateMutation::SplitPane {
                                direction,
                                ratio: even_ratio,
                                command: pane.resolved_command(),
                                cwd: pane.cwd.clone(),
                                title: pane.title.clone(),
                                theme: pane.theme.clone(),
                            });
                        }
                    }
                }
                "tiled" => {
                    // Balanced grid: alternate H/V splits
                    for (i, pane) in window.panes.iter().enumerate() {
                        if i == 0 {
                            operations.push(self.pane_create_op(pane, win_idx));
                        } else {
                            let direction = if i % 2 == 1 {
                                SplitDirection::Horizontal
                            } else {
                                SplitDirection::Vertical
                            };
                            operations.push(StateMutation::SplitPane {
                                direction,
                                ratio: 0.5,
                                command: pane.resolved_command(),
                                cwd: pane.cwd.clone(),
                                title: pane.title.clone(),
                                theme: pane.theme.clone(),
                            });
                        }
                    }
                }
                _ => unreachable!("Layout validated in validate()"),
            }
        }

        // 3. Submit as atomic transaction
        state
            .apply_transaction(operations)
            .await
            .map_err(|e| TemplateError::ApplicationError(e.to_string()))
    }

    fn pane_create_op(
        &self,
        pane: &ResolvedPaneSpec,
        _win_idx: usize,
    ) -> StateMutation {
        StateMutation::SetInitialPane {
            command: pane.resolved_command(),
            cwd: pane.cwd.clone(),
            title: pane.title.clone(),
            theme: pane.theme.clone(),
        }
    }
}

impl ResolvedPaneSpec {
    /// Get the command to run, preferring the array form.
    fn resolved_command(&self) -> Option<Vec<String>> {
        if let Some(ref cmd) = self.command {
            Some(cmd.clone())
        } else if let Some(ref shell_cmd) = self.shell_command {
            // Wrap in shell invocation
            Some(vec!["sh".into(), "-c".into(), shell_cmd.clone()])
        } else {
            None // Default shell
        }
    }
}
```

### Step 4: Implement CLI `shux apply` Command

Create `crates/shux/src/commands/apply.rs`:

```rust
//! CLI command: shux apply <template.toml> [--var key=value ...]
//!
//! Applies a session template to create a complete workspace.

use clap::Args;
use shux_core::template::{SessionTemplate, VarContext};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct ApplyArgs {
    /// Path to the template TOML file.
    /// Can be an absolute path or a name that resolves to
    /// ~/.config/shux/templates/<name>.toml
    pub template: String,

    /// Template variables: --var key=value (can be repeated)
    #[arg(long = "var", value_parser = parse_var)]
    pub vars: Vec<(String, String)>,

    /// Attach to the session after creation
    #[arg(long, default_value_t = false)]
    pub attach: bool,

    /// Dry run: validate and resolve template without applying
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
}

/// Parse a key=value pair.
fn parse_var(s: &str) -> Result<(String, String), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("Invalid variable format: '{s}'. Expected key=value"))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}

/// Resolve the template file path.
///
/// If the path exists as-is, use it. Otherwise, look in the templates directory.
fn resolve_template_path(template: &str) -> Result<PathBuf, anyhow::Error> {
    let direct = PathBuf::from(template);
    if direct.exists() {
        return Ok(direct);
    }

    // Try with .toml extension
    let with_ext = direct.with_extension("toml");
    if with_ext.exists() {
        return Ok(with_ext);
    }

    // Look in ~/.config/shux/templates/
    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("shux")
        .join("templates");

    let in_config = config_dir.join(template);
    if in_config.exists() {
        return Ok(in_config);
    }

    let in_config_with_ext = config_dir.join(format!("{template}.toml"));
    if in_config_with_ext.exists() {
        return Ok(in_config_with_ext);
    }

    anyhow::bail!(
        "Template not found: '{template}'. Searched:\n  \
         - {}\n  \
         - {}\n  \
         - {}\n  \
         - {}",
        direct.display(),
        with_ext.display(),
        in_config.display(),
        in_config_with_ext.display(),
    )
}

pub async fn execute(args: ApplyArgs) -> Result<(), anyhow::Error> {
    // 1. Resolve template path
    let path = resolve_template_path(&args.template)?;
    tracing::info!(path = %path.display(), "Loading template");

    // 2. Parse template
    let template = SessionTemplate::load(&path)?;
    println!(
        "Template: {} — {}",
        template.template.name, template.template.description
    );

    // 3. Build variable context
    let cli_vars: HashMap<String, String> = args.vars.into_iter().collect();
    let var_ctx = VarContext::new(cli_vars);

    // 4. Resolve variables
    let resolved = template.resolve(&var_ctx)?;
    println!("Session: {}", resolved.session_name);
    for (i, window) in resolved.windows.iter().enumerate() {
        println!(
            "  Window {}: {} ({} layout, {} panes)",
            i + 1,
            window.title,
            window.layout,
            window.panes.len()
        );
    }

    // 5. Dry run: stop here
    if args.dry_run {
        println!("\nDry run: template validated and resolved successfully.");
        return Ok(());
    }

    // 6. Apply template via state.apply transaction
    // Connect to daemon and submit
    let client = connect_to_daemon().await?;
    let result = client
        .call("state.apply", serde_json::to_value(&resolved)?)
        .await?;

    println!(
        "\nSession '{}' created with {} windows.",
        resolved.session_name,
        resolved.windows.len()
    );

    // 7. Attach if requested
    if args.attach {
        println!("Attaching...");
        // Invoke the attach flow
        crate::commands::attach::execute_attach(&resolved.session_name).await?;
    }

    Ok(())
}
```

### Step 5: Add Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_TEMPLATE: &str = r#"
[template]
name = "web-project"
description = "Frontend + Backend + Tests layout"

[session]
name = "{{project_name}}"
cwd = "{{project_dir}}"

[[windows]]
title = "editor"
layout = "single"

[[windows.panes]]
command = ["nvim", "."]

[[windows]]
title = "servers"
layout = "vertical"

[[windows.panes]]
command = ["npm", "run", "dev"]
title = "frontend"

[[windows.panes]]
command = ["cargo", "watch", "-x", "run"]
title = "backend"
theme = "prod"

[[windows]]
title = "logs"
layout = "horizontal"

[[windows.panes]]
shell_command = "tail -f /var/log/app.log"
title = "app"

[[windows.panes]]
shell_command = "tail -f /var/log/error.log"
title = "errors"
"#;

    #[test]
    fn test_parse_template() {
        let template = SessionTemplate::parse(SAMPLE_TEMPLATE).unwrap();
        assert_eq!(template.template.name, "web-project");
        assert_eq!(template.windows.len(), 3);
        assert_eq!(template.windows[0].layout, "single");
        assert_eq!(template.windows[1].layout, "vertical");
        assert_eq!(template.windows[1].panes.len(), 2);
    }

    #[test]
    fn test_resolve_variables() {
        let template = SessionTemplate::parse(SAMPLE_TEMPLATE).unwrap();

        let mut cli_vars = HashMap::new();
        cli_vars.insert("project_name".to_string(), "shux".to_string());
        cli_vars.insert("project_dir".to_string(), "/home/user/shux".to_string());

        let ctx = VarContext::new(cli_vars);
        let resolved = template.resolve(&ctx).unwrap();

        assert_eq!(resolved.session_name, "shux");
        assert_eq!(resolved.session_cwd.as_deref(), Some("/home/user/shux"));
    }

    #[test]
    fn test_unresolved_variables_error() {
        let template = SessionTemplate::parse(SAMPLE_TEMPLATE).unwrap();

        // No variables provided — should fail
        let ctx = VarContext::new(HashMap::new());
        let result = template.resolve(&ctx);

        assert!(result.is_err());
        if let Err(TemplateError::UnresolvedVariables { variables }) = result {
            assert!(variables.contains(&"project_name".to_string()));
            assert!(variables.contains(&"project_dir".to_string()));
        } else {
            panic!("Expected UnresolvedVariables error");
        }
    }

    #[test]
    fn test_env_var_resolution() {
        std::env::set_var("SHUX_TPL_PROJECT_NAME", "env-project");
        std::env::set_var("SHUX_TPL_PROJECT_DIR", "/env/path");

        let template = SessionTemplate::parse(SAMPLE_TEMPLATE).unwrap();
        let ctx = VarContext::new(HashMap::new());
        let resolved = template.resolve(&ctx).unwrap();

        assert_eq!(resolved.session_name, "env-project");
        assert_eq!(resolved.session_cwd.as_deref(), Some("/env/path"));

        std::env::remove_var("SHUX_TPL_PROJECT_NAME");
        std::env::remove_var("SHUX_TPL_PROJECT_DIR");
    }

    #[test]
    fn test_cli_vars_override_env() {
        std::env::set_var("SHUX_TPL_PROJECT_NAME", "env-project");

        let template = SessionTemplate::parse(SAMPLE_TEMPLATE).unwrap();
        let mut cli_vars = HashMap::new();
        cli_vars.insert("project_name".to_string(), "cli-project".to_string());
        cli_vars.insert("project_dir".to_string(), "/cli/path".to_string());

        let ctx = VarContext::new(cli_vars);
        let resolved = template.resolve(&ctx).unwrap();

        assert_eq!(resolved.session_name, "cli-project"); // CLI wins
        std::env::remove_var("SHUX_TPL_PROJECT_NAME");
    }

    #[test]
    fn test_builtin_variables() {
        let template_str = r#"
[template]
name = "test"
description = ""

[session]
name = "session-{{user}}"

[[windows]]
title = "work"
layout = "single"
"#;

        let template = SessionTemplate::parse(template_str).unwrap();
        let ctx = VarContext::new(HashMap::new());
        let resolved = template.resolve(&ctx).unwrap();

        let user = std::env::var("USER").unwrap_or("unknown".into());
        assert_eq!(resolved.session_name, format!("session-{user}"));
    }

    #[test]
    fn test_invalid_layout_rejected() {
        let template_str = r#"
[template]
name = "bad"
description = ""

[session]
name = "test"

[[windows]]
title = "win"
layout = "invalid-layout"
"#;

        let result = SessionTemplate::parse(template_str);
        assert!(matches!(result, Err(TemplateError::InvalidLayout { .. })));
    }

    #[test]
    fn test_single_layout_multiple_panes_rejected() {
        let template_str = r#"
[template]
name = "bad"
description = ""

[session]
name = "test"

[[windows]]
title = "win"
layout = "single"

[[windows.panes]]
command = ["bash"]

[[windows.panes]]
command = ["zsh"]
"#;

        let result = SessionTemplate::parse(template_str);
        assert!(matches!(result, Err(TemplateError::ValidationError(_))));
    }

    #[test]
    fn test_shell_command_wrapping() {
        let pane = ResolvedPaneSpec {
            command: None,
            shell_command: Some("echo hello | grep hello".into()),
            title: None,
            cwd: None,
            theme: None,
            ratio: 0.5,
        };

        let cmd = pane.resolved_command().unwrap();
        assert_eq!(cmd, vec!["sh", "-c", "echo hello | grep hello"]);
    }

    #[test]
    fn test_empty_template_rejected() {
        let template_str = r#"
[template]
name = "empty"
description = ""

[session]
name = "test"
"#;

        let result = SessionTemplate::parse(template_str);
        assert!(matches!(result, Err(TemplateError::ValidationError(_))));
    }

    #[test]
    fn test_parse_var_cli() {
        assert_eq!(
            parse_var("key=value").unwrap(),
            ("key".to_string(), "value".to_string())
        );
        assert_eq!(
            parse_var("key=val=ue").unwrap(),
            ("key".to_string(), "val=ue".to_string())
        );
        assert!(parse_var("no-equals").is_err());
    }

    #[test]
    fn test_template_with_no_variables_resolves() {
        let template_str = r#"
[template]
name = "simple"
description = "No variables"

[session]
name = "fixed-name"

[[windows]]
title = "editor"
layout = "single"

[[windows.panes]]
command = ["vim"]
"#;

        let template = SessionTemplate::parse(template_str).unwrap();
        let ctx = VarContext::new(HashMap::new());
        let resolved = template.resolve(&ctx).unwrap();
        assert_eq!(resolved.session_name, "fixed-name");
    }
}
```

---

## Verification

### Functional

```bash
# Build the workspace
cargo build --workspace

# Verify template module compiles
cargo check -p shux-core
cargo check -p shux

# Dry run a template
shux apply docs/examples/web-project.toml --var project_name=myapp --var project_dir=/tmp/myapp --dry-run
# Expected: prints resolved template without creating anything

# Apply a template
shux apply web-project --var project_name=myapp --var project_dir=. --attach
# Expected: creates session "myapp" with defined windows and panes

# Missing variables produce actionable error
shux apply web-project
# Expected: error listing unresolved variables: project_name, project_dir
```

### Tests

```bash
# Run template tests
cargo nextest run -p shux-core -- template

# Expected passing tests:
# - test_parse_template
# - test_resolve_variables
# - test_unresolved_variables_error
# - test_env_var_resolution
# - test_cli_vars_override_env
# - test_builtin_variables
# - test_invalid_layout_rejected
# - test_single_layout_multiple_panes_rejected
# - test_shell_command_wrapping
# - test_empty_template_rejected
# - test_parse_var_cli
# - test_template_with_no_variables_resolves
```

---

## Completion Criteria

- [ ] Template TOML format implemented: [template], [session], [[windows]], [[windows.panes]] sections
- [ ] Template metadata: name, description, version fields
- [ ] Session spec: name, cwd, theme (all support {{variables}})
- [ ] Window spec: title, layout, panes, theme
- [ ] Pane spec: command (array), shell_command (string), title, cwd, theme, ratio
- [ ] Layout values validated: single, vertical, horizontal, even-vertical, even-horizontal, tiled
- [ ] Mustache-style `{{var}}` substitution (no logic, no loops)
- [ ] Variable resolution priority: CLI --var > SHUX_TPL_* env > built-in defaults
- [ ] Built-in variables: {{cwd}}, {{user}}, {{home}}, {{hostname}}
- [ ] Missing required variables produce actionable error listing all unresolved
- [ ] `shux apply <template.toml> [--var key=value ...]` CLI command
- [ ] Template path resolution: direct path, with .toml extension, ~/.config/shux/templates/
- [ ] `--dry-run` flag validates and resolves without applying
- [ ] `--attach` flag attaches to the session after creation
- [ ] Application creates session + windows + panes atomically via state.apply transaction
- [ ] Layout algorithms: single (1 pane), vertical/horizontal (split), even-* (equal splits), tiled (grid)
- [ ] shell_command panes wrapped in `sh -c "<command>"`
- [ ] Validation: empty templates, invalid layouts, single layout with multiple panes, invalid ratios
- [ ] Unit tests pass for parsing, variable resolution, validation, and edge cases

---

## Commit Message

```
feat(core,cli): add declarative session templates with variable substitution

- Define SessionTemplate TOML format: [template], [session], [[windows]], [[windows.panes]]
- Implement Mustache-style {{var}} substitution with three-tier resolution:
  CLI --var flags > SHUX_TPL_* env vars > built-in defaults (cwd, user, home)
- Missing variables produce actionable error listing all unresolved
- Layout algorithms: single, vertical, horizontal, even-vertical/horizontal, tiled
- `shux apply` CLI command with --dry-run and --attach flags
- Template path resolution: direct, .toml extension, ~/.config/shux/templates/
- Atomic session creation via state.apply transaction
```

---

## Session Protocol

1. **Before starting:** Read task 022 (TOML config system) for config parsing patterns. Read task 015 (pane operations) for the split/create mutation types. Read PRD section 10.3 for the exact template format specification. Read task 008 (JSON-RPC server) for the state.apply transaction pattern.
2. **During:** Implement in order: data model (Step 1), variable resolution (Step 2), template application (Step 3), CLI command (Step 4), tests (Step 5). Run `cargo check` after each step. Create a sample template file for manual testing.
3. **Edge cases to watch for:**
   - Template with no panes in a window (should get default shell)
   - Template with variables in command arguments (`["nvim", "{{project_dir}}/src"]`)
   - Template variable names with underscores and numbers
   - TOML array-of-tables syntax for windows.panes (easy to get wrong)
   - CWD that does not exist yet (should still create the pane, cd will fail gracefully)
   - Template referencing a theme that is not installed (warn but do not fail)
   - Nested variable references are not supported ({{{{var}}}} should not recurse)
4. **After:** Run full test suite. Create 2-3 example template files in `docs/examples/`. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings.
