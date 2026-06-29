use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};
use serde::Deserialize;

#[derive(Clone, Debug)]
pub struct ResolvedPluginPackage {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub expected_name: Option<String>,
    pub expected_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    plugin: ManifestPlugin,
    entry: ManifestEntry,
    #[serde(default)]
    platform: ManifestPlatform,
    #[serde(default)]
    commands: BTreeMap<String, ManifestCommand>,
}

#[derive(Debug, Deserialize)]
struct ManifestPlugin {
    name: String,
    id: String,
    version: String,
    runtime: String,
}

#[derive(Debug, Deserialize)]
struct ManifestEntry {
    command: String,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ManifestPlatform {
    #[serde(default)]
    os: Vec<String>,
    #[serde(default)]
    arch: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ManifestCommand {
    entry: String,
}

pub fn resolve_plugin_package(path: &Path) -> anyhow::Result<ResolvedPluginPackage> {
    if !path.is_dir() {
        let command = if path.exists() {
            let command = path
                .canonicalize()
                .with_context(|| format!("canonicalize plugin executable {}", path.display()))?;
            validate_executable(&command)?;
            command
        } else {
            path.to_path_buf()
        };
        return Ok(ResolvedPluginPackage {
            command,
            args: Vec::new(),
            cwd: None,
            expected_name: None,
            expected_version: None,
        });
    }

    let canonical_root = path
        .canonicalize()
        .with_context(|| format!("canonicalize plugin directory {}", path.display()))?;
    let manifest_path = canonical_root.join("shux-plugin.toml");
    if !manifest_path.is_file() {
        let legacy_entrypoint = canonical_root.join("plugin.sh");
        if legacy_entrypoint.is_file() {
            validate_executable(&legacy_entrypoint)?;
            return Ok(ResolvedPluginPackage {
                command: canonical_entrypoint_under_root(
                    &canonical_root,
                    &legacy_entrypoint,
                    "plugin.sh",
                )?,
                args: Vec::new(),
                cwd: Some(canonical_root),
                expected_name: None,
                expected_version: None,
            });
        }
        bail!(
            "plugin directory {} is missing shux-plugin.toml or plugin.sh",
            path.display()
        );
    }

    let raw = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: Manifest =
        toml::from_str(&raw).with_context(|| format!("parse {}", manifest_path.display()))?;
    let command = validate_manifest(&canonical_root, &manifest)?;

    Ok(ResolvedPluginPackage {
        command,
        args: manifest.entry.args,
        cwd: Some(canonical_root),
        expected_name: Some(manifest.plugin.name),
        expected_version: Some(manifest.plugin.version),
    })
}

fn validate_manifest(root: &Path, manifest: &Manifest) -> anyhow::Result<PathBuf> {
    validate_name("plugin.name", &manifest.plugin.name)?;
    validate_id("plugin.id", &manifest.plugin.id)?;
    if manifest.plugin.version.trim().is_empty() {
        bail!("plugin.version must not be empty");
    }
    if manifest.plugin.runtime != "process" {
        bail!("only process runtime plugins are supported in v0.5");
    }
    validate_relative_entrypoint("entry.command", &manifest.entry.command)?;
    validate_platform(&manifest.platform)?;

    let command = root.join(&manifest.entry.command);
    if !command.is_file() {
        bail!("plugin entrypoint {} does not exist", command.display());
    }
    validate_executable(&command)?;
    let command = canonical_entrypoint_under_root(root, &command, "entry.command")?;

    for (name, command_manifest) in &manifest.commands {
        validate_name("commands key", name)?;
        if is_reserved_command_name(name) {
            bail!("commands key '{name}' is reserved by shux");
        }
        validate_relative_entrypoint("commands.entry", &command_manifest.entry)?;
        let command = root.join(&command_manifest.entry);
        if command.exists() {
            let _ = canonical_entrypoint_under_root(root, &command, "commands.entry")?;
        }
    }

    Ok(command)
}

fn validate_name(field: &str, value: &str) -> anyhow::Result<()> {
    if value.trim().is_empty() {
        bail!("{field} must not be empty");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        bail!("{field} may contain only ASCII letters, numbers, '-' and '_'");
    }
    Ok(())
}

fn validate_id(field: &str, value: &str) -> anyhow::Result<()> {
    if value.trim().is_empty() {
        bail!("{field} must not be empty");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
    {
        bail!("{field} may contain only ASCII letters, numbers, '.', '-' and '_'");
    }
    Ok(())
}

pub(super) fn is_reserved_command_name(name: &str) -> bool {
    matches!(
        name,
        "api"
            | "attach"
            | "completion"
            | "config"
            | "events"
            | "help"
            | "pane"
            | "plugin"
            | "rpc"
            | "session"
            | "state"
            | "version"
            | "window"
            | "audit"
            | "create"
            | "grant"
            | "grants"
            | "init"
            | "install"
            | "kill"
            | "list"
            | "ls"
            | "reload"
            | "revoke"
            | "scaffold"
            | "stop"
            | "uninstall"
    )
}

fn validate_relative_entrypoint(field: &str, value: &str) -> anyhow::Result<()> {
    if value.trim().is_empty() {
        bail!("{field} must not be empty");
    }
    let path = Path::new(value);
    if path.is_absolute() {
        bail!("{field} must be relative");
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        bail!("{field} must not escape the plugin directory");
    }
    Ok(())
}

fn validate_platform(platform: &ManifestPlatform) -> anyhow::Result<()> {
    let current_os = std::env::consts::OS;
    let current_arch = std::env::consts::ARCH;
    if !platform.os.is_empty() && !platform.os.iter().any(|os| os == current_os) {
        bail!("plugin does not support current OS {current_os}");
    }
    if !platform.arch.is_empty() && !platform.arch.iter().any(|arch| arch == current_arch) {
        bail!("plugin does not support current architecture {current_arch}");
    }
    Ok(())
}

fn canonical_entrypoint_under_root(
    root: &Path,
    entrypoint: &Path,
    field: &str,
) -> anyhow::Result<PathBuf> {
    let canonical_entrypoint = entrypoint
        .canonicalize()
        .with_context(|| format!("canonicalize {}", entrypoint.display()))?;
    if !canonical_entrypoint.starts_with(root) {
        bail!("{field} must resolve inside the plugin directory");
    }
    Ok(canonical_entrypoint)
}

#[cfg(unix)]
fn validate_executable(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .permissions()
        .mode();
    if mode & 0o111 == 0 {
        bail!("plugin entrypoint {} is not executable", path.display());
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_executable(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_plugin(root: &Path, manifest_extra: &str) {
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let entry = root.join("bin/hello.sh");
        std::fs::write(&entry, "#!/usr/bin/env bash\nprintf '%s\\n' hello\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&entry).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&entry, perms).unwrap();
        }
        std::fs::write(
            root.join("shux-plugin.toml"),
            format!(
                r#"[plugin]
name = "hello"
id = "dev.shux.hello"
version = "0.1.0"
runtime = "process"

[entry]
command = "bin/hello.sh"
args = ["--from-manifest"]

{manifest_extra}
"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn resolves_directory_manifest_entrypoint() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "");

        let resolved = resolve_plugin_package(tmp.path()).unwrap();
        assert_eq!(
            resolved.command,
            tmp.path().join("bin/hello.sh").canonicalize().unwrap()
        );
        assert_eq!(resolved.args, ["--from-manifest"]);
        assert_eq!(resolved.cwd, Some(tmp.path().canonicalize().unwrap()));
        assert_eq!(resolved.expected_name.as_deref(), Some("hello"));
        assert_eq!(resolved.expected_version.as_deref(), Some("0.1.0"));
    }

    #[test]
    fn rejects_escaping_entrypoint() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "");
        let manifest = std::fs::read_to_string(tmp.path().join("shux-plugin.toml"))
            .unwrap()
            .replace("bin/hello.sh", "../outside.sh");
        std::fs::write(tmp.path().join("shux-plugin.toml"), manifest).unwrap();

        let err = resolve_plugin_package(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("escape"));
    }

    #[test]
    fn rejects_symlink_entrypoint_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(outside.path()).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(outside.path(), perms).unwrap();
        }
        write_plugin(tmp.path(), "");
        std::fs::remove_file(tmp.path().join("bin/hello.sh")).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), tmp.path().join("bin/hello.sh")).unwrap();
        }
        #[cfg(not(unix))]
        {
            return;
        }

        let err = resolve_plugin_package(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("inside the plugin directory"));
    }

    #[test]
    fn rejects_dotted_plugin_name_before_spawn() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "");
        let manifest = std::fs::read_to_string(tmp.path().join("shux-plugin.toml"))
            .unwrap()
            .replace("name = \"hello\"", "name = \"hello.world\"");
        std::fs::write(tmp.path().join("shux-plugin.toml"), manifest).unwrap();

        let err = resolve_plugin_package(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("plugin.name"));
    }

    #[test]
    fn rejects_reserved_manifest_command_aliases() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(
            tmp.path(),
            r#"[commands."session"]
entry = "bin/hello.sh"
"#,
        );

        let err = resolve_plugin_package(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("reserved"));
    }

    #[test]
    fn rejects_missing_required_manifest_fields() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "");

        for (needle, replacement, expected) in [
            ("name = \"hello\"\n", "", "missing field `name`"),
            ("id = \"dev.shux.hello\"\n", "", "missing field `id`"),
            (
                "command = \"bin/hello.sh\"\n",
                "",
                "missing field `command`",
            ),
        ] {
            write_plugin(tmp.path(), "");
            let manifest = std::fs::read_to_string(tmp.path().join("shux-plugin.toml"))
                .unwrap()
                .replace(needle, replacement);
            std::fs::write(tmp.path().join("shux-plugin.toml"), manifest).unwrap();

            let err = resolve_plugin_package(tmp.path()).unwrap_err();
            let chain = err
                .chain()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                chain.contains(expected),
                "expected {expected:?}, got {chain}"
            );
        }
    }

    #[test]
    fn rejects_unsupported_platform_before_spawn() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "");
        let manifest = std::fs::read_to_string(tmp.path().join("shux-plugin.toml"))
            .unwrap()
            .replace(
                r#"[entry]
command = "bin/hello.sh"
args = ["--from-manifest"]"#,
                r#"[entry]
command = "bin/hello.sh"
args = ["--from-manifest"]

[platform]
os = ["definitely-not-this-os"]"#,
            );
        std::fs::write(tmp.path().join("shux-plugin.toml"), manifest).unwrap();

        let err = resolve_plugin_package(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("current OS"));
    }

    #[test]
    fn rejects_missing_manifest_in_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let err = resolve_plugin_package(tmp.path()).unwrap_err();
        assert!(
            err.to_string()
                .contains("missing shux-plugin.toml or plugin.sh")
        );
    }

    #[test]
    fn preserves_legacy_plugin_sh_directory_install() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = tmp.path().join("plugin.sh");
        std::fs::write(&entry, "#!/usr/bin/env bash\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&entry).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&entry, perms).unwrap();
        }

        let resolved = resolve_plugin_package(tmp.path()).unwrap();
        assert_eq!(resolved.command, entry.canonicalize().unwrap());
        assert!(resolved.args.is_empty());
    }
}
