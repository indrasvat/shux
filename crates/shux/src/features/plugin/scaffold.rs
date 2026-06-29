use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::ValueEnum;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum PluginScaffoldRuntime {
    Sh,
}

impl PluginScaffoldRuntime {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sh => "sh",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ScaffoldOptions {
    pub runtime: PluginScaffoldRuntime,
    pub name: Option<String>,
    pub id: Option<String>,
    pub force: bool,
}

#[derive(Clone, Debug)]
pub struct ScaffoldReport {
    pub root: PathBuf,
    pub name: String,
    pub id: String,
    pub entrypoint: PathBuf,
}

pub fn scaffold_plugin(path: &Path, options: &ScaffoldOptions) -> anyhow::Result<ScaffoldReport> {
    match options.runtime {
        PluginScaffoldRuntime::Sh => {}
    }

    let root = path.to_path_buf();
    let default_name = root
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("shux-plugin");
    let name = options
        .name
        .clone()
        .unwrap_or_else(|| sanitize_name(default_name));
    validate_name(&name)?;

    let id = options
        .id
        .clone()
        .unwrap_or_else(|| format!("local.shux.{name}"));
    validate_id(&id)?;

    if root.exists() {
        let mut entries = fs::read_dir(&root)
            .with_context(|| format!("read plugin directory {}", root.display()))?;
        if entries.next().is_some() && !options.force {
            bail!(
                "plugin directory {} is not empty; pass --force to scaffold into it",
                root.display()
            );
        }
    }

    fs::create_dir_all(root.join("bin"))
        .with_context(|| format!("create plugin directory {}", root.display()))?;

    let entrypoint_rel = PathBuf::from("bin").join(format!("{name}.sh"));
    let entrypoint = root.join(&entrypoint_rel);
    write_new_file(
        &root.join("shux-plugin.toml"),
        &render_manifest(&name, &id, &entrypoint_rel),
        options.force,
    )?;
    write_new_file(
        &root.join("README.md"),
        &render_readme(&name),
        options.force,
    )?;
    write_new_file(&root.join("LICENSE"), render_license(), options.force)?;
    write_new_file(&entrypoint, &render_sh_entrypoint(&name), options.force)?;
    make_executable(&entrypoint)?;

    Ok(ScaffoldReport {
        root,
        name,
        id,
        entrypoint,
    })
}

fn write_new_file(path: &Path, contents: &str, force: bool) -> anyhow::Result<()> {
    if path.exists() && !force {
        bail!("refusing to overwrite existing file {}", path.display());
    }
    fs::write(path, contents).with_context(|| format!("write {}", path.display()))
}

fn sanitize_name(raw: &str) -> String {
    let mut name = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            name.push(ch.to_ascii_lowercase());
        } else {
            name.push('-');
        }
    }
    let trimmed = name.trim_matches('-');
    if trimmed.is_empty() {
        "shux-plugin".to_string()
    } else {
        trimmed.to_string()
    }
}

fn validate_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        bail!("plugin name must not be empty");
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        bail!("plugin name may contain only ASCII letters, numbers, '-' and '_'");
    }
    if super::package::is_reserved_command_name(name) {
        bail!("plugin name '{name}' is reserved by shux");
    }
    Ok(())
}

fn validate_id(id: &str) -> anyhow::Result<()> {
    if id.is_empty() {
        bail!("plugin id must not be empty");
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
    {
        bail!("plugin id may contain only ASCII letters, numbers, '.', '-' and '_'");
    }
    Ok(())
}

fn render_manifest(name: &str, id: &str, entrypoint_rel: &Path) -> String {
    let entry = entrypoint_rel;
    let entry = entry.to_string_lossy().replace('\\', "/");
    format!(
        r#"[plugin]
name = "{name}"
id = "{id}"
version = "0.1.0"
description = "A local Shux process plugin"
runtime = "process"

[entry]
command = "{entry}"
args = []

[platform]
os = ["macos", "linux"]
arch = ["aarch64", "x86_64"]

[commands."{name}"]
usage = "shux plugin install ."
description = "Install and run the {name} plugin from this package"
entry = "{entry}"
"#
    )
}

fn render_readme(name: &str) -> String {
    format!(
        r#"# {name}

Local Shux process plugin.

## Run

```bash
shux plugin install .
shux plugin list
shux plugin stop {name}
```

The generated manifest points at `./bin/{name}.sh`, so installing the directory
validates package metadata before the plugin process is spawned.
"#
    )
}

fn render_license() -> &'static str {
    "MIT License\n\nCopyright (c) 2026\n\nPermission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the \"Software\"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so.\n\nTHE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES, OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF, OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.\n"
}

fn render_sh_entrypoint(name: &str) -> String {
    let json_name = serde_json::to_string(name).expect("plugin name serializes");
    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

IFS= read -r _init || exit 1
printf '{{"jsonrpc":"2.0","id":"init","result":{{"name":{json_name},"version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}}}\n'

while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*) exit 0 ;;
  esac
done
"#
    )
}

#[cfg(unix)]
fn make_executable(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut perms = fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).with_context(|| format!("chmod {}", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffolds_sh_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("hello");
        let report = scaffold_plugin(
            &root,
            &ScaffoldOptions {
                runtime: PluginScaffoldRuntime::Sh,
                name: Some("hello".into()),
                id: Some("dev.shux.hello".into()),
                force: false,
            },
        )
        .unwrap();

        assert_eq!(report.name, "hello");
        assert_eq!(report.id, "dev.shux.hello");
        assert!(root.join("shux-plugin.toml").is_file());
        assert!(root.join("README.md").is_file());
        assert!(root.join("LICENSE").is_file());
        assert!(report.entrypoint.is_file());
        let manifest = fs::read_to_string(root.join("shux-plugin.toml")).unwrap();
        assert!(manifest.contains("command = \"bin/hello.sh\""));
    }

    #[test]
    fn refuses_non_empty_directory_without_force() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("hello");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("existing.txt"), "owned").unwrap();

        let err = scaffold_plugin(
            &root,
            &ScaffoldOptions {
                runtime: PluginScaffoldRuntime::Sh,
                name: Some("hello".into()),
                id: None,
                force: false,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("not empty"));
    }

    #[test]
    fn refuses_reserved_plugin_name_before_writing_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("reserved");

        let err = scaffold_plugin(
            &root,
            &ScaffoldOptions {
                runtime: PluginScaffoldRuntime::Sh,
                name: Some("session".into()),
                id: None,
                force: false,
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("reserved by shux"));
        assert!(!root.join("shux-plugin.toml").exists());
    }
}
