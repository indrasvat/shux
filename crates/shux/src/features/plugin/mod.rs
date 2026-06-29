pub mod package;
pub mod scaffold;

use std::path::Path;

pub use package::resolve_plugin_package;
pub use scaffold::{PluginScaffoldRuntime, ScaffoldOptions, scaffold_plugin};

pub async fn dispatch(
    command: crate::cli::PluginCommand,
    socket_path: &Path,
    format: crate::cli::OutputFormat,
) -> anyhow::Result<()> {
    match command {
        crate::cli::PluginCommand::Scaffold {
            path,
            runtime,
            name,
            id,
            force,
        }
        | crate::cli::PluginCommand::Create {
            path,
            runtime,
            name,
            id,
            force,
        } => crate::cli::handle_plugin_scaffold(&path, runtime, name, id, force, format),
        crate::cli::PluginCommand::Init {
            runtime,
            name,
            id,
            force,
        } => {
            let path = std::env::current_dir()?;
            crate::cli::handle_plugin_scaffold(&path, runtime, name, id, force, format)
        }
        crate::cli::PluginCommand::Install {
            path,
            args,
            cwd,
            no_watch,
        } => {
            let mut stream = crate::client::ensure_daemon_running_at(socket_path).await?;
            crate::cli::handle_plugin_install(
                &mut stream,
                &path,
                &args,
                cwd.as_deref(),
                !no_watch,
                format,
            )
            .await
        }
        crate::cli::PluginCommand::List => {
            let mut stream = crate::client::ensure_daemon_running_at(socket_path).await?;
            crate::cli::handle_plugin_list(&mut stream, format).await
        }
        crate::cli::PluginCommand::Kill { name } => {
            let mut stream = crate::client::ensure_daemon_running_at(socket_path).await?;
            crate::cli::handle_plugin_kill(&mut stream, &name, format).await
        }
        crate::cli::PluginCommand::Stop { name } => {
            let mut stream = crate::client::ensure_daemon_running_at(socket_path).await?;
            crate::cli::handle_plugin_stop(&mut stream, &name, format).await
        }
        crate::cli::PluginCommand::Reload { name } => {
            let mut stream = crate::client::ensure_daemon_running_at(socket_path).await?;
            crate::cli::handle_plugin_reload(&mut stream, &name, format).await
        }
        crate::cli::PluginCommand::Grant {
            plugin,
            method,
            target,
            subscribe,
        } => {
            let mut stream = crate::client::ensure_daemon_running_at(socket_path).await?;
            crate::cli::handle_plugin_grant(
                &mut stream,
                &plugin,
                &method,
                target.as_deref(),
                subscribe,
                format,
            )
            .await
        }
        crate::cli::PluginCommand::Revoke {
            plugin,
            method,
            target,
            subscribe,
        } => {
            let mut stream = crate::client::ensure_daemon_running_at(socket_path).await?;
            crate::cli::handle_plugin_revoke(
                &mut stream,
                &plugin,
                &method,
                target.as_deref(),
                subscribe,
                format,
            )
            .await
        }
        crate::cli::PluginCommand::Grants { plugin } => {
            let mut stream = crate::client::ensure_daemon_running_at(socket_path).await?;
            crate::cli::handle_plugin_grants(&mut stream, &plugin, format).await
        }
        crate::cli::PluginCommand::Audit { plugin, tail } => {
            let mut stream = crate::client::ensure_daemon_running_at(socket_path).await?;
            crate::cli::handle_plugin_audit(&mut stream, &plugin, tail, format).await
        }
    }
}
