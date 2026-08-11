//! The long-form `--help` prose: the banner blurb and the agent reference.
//!
//! Emitted twice — brand-coloured for a terminal, plain for a pipe — so
//! `NO_COLOR` and a redirected stdout produce clean text.

use clap::builder::styling::{AnsiColor, Effects, Styles};

pub const CLAP_STYLES: Styles = Styles::styled()
    .header(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Yellow.on_default())
    .valid(AnsiColor::Green.on_default())
    .invalid(AnsiColor::Red.on_default().effects(Effects::BOLD))
    .error(AnsiColor::Red.on_default().effects(Effects::BOLD));

/// Render the long-form agent reference block appended to `shux --help`.
///
/// The same content is emitted twice — once with shux's brand colours
/// baked in via ANSI escapes (terracotta accent for headers + `shux`
/// commands, green for RPC methods, dim for inline comments), and once
/// as plain text with all escapes stripped. The colour decision honours
/// `NO_COLOR=…` (any value) and falls back to plain when stdout isn't
/// a TTY, matching the same `IsTerminal` check the rest of the CLI uses.
/// Long-form `about` text shown at the top of `shux --help`. Adapts to
/// NO_COLOR + IsTerminal so plain piped output stays clean; brand-tinted
/// when emitted to a real terminal. Returns plain text with optional ANSI
/// escapes embedded.
pub fn long_about() -> String {
    use std::io::IsTerminal;
    let force =
        std::env::var_os("CLICOLOR_FORCE").is_some() || std::env::var_os("FORCE_COLOR").is_some();
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let colorize = !no_color && (force || std::io::stdout().is_terminal());
    render_long_about(colorize)
}

pub fn render_long_about(colorize: bool) -> String {
    let acc = if colorize {
        "\x1b[1;38;2;215;108;58m"
    } else {
        ""
    }; // bold terracotta
    let dim = if colorize { "\x1b[2m" } else { "" };
    let bold = if colorize { "\x1b[1m" } else { "" };
    let mono = if colorize {
        "\x1b[38;2;180;175;160m"
    } else {
        ""
    }; // warm pale gray for inline code
    let r = if colorize { "\x1b[0m" } else { "" };

    let sx = format!("{acc}shux{r}");
    let bul = format!("{dim}·{r}");

    let mut s = String::with_capacity(512);
    s.push_str(&format!(
        "{sx} is a terminal multiplexer (sessions / windows / panes, like tmux) \
        for humans and AI agents.\n\n"
    ));
    s.push_str(&format!(
        "{bold}Typed JSON-RPC surface (UDS + TCP) with:{r}\n"
    ));
    s.push_str(&format!("  {bul} atomic declarative workspace templates\n"));
    s.push_str(&format!("  {bul} optimistic concurrency on every entity\n"));
    s.push_str(&format!("  {bul} sealed PTY-output event bus\n"));
    s.push_str(&format!(
        "  {bul} built-in PNG rasterizer {dim}— any pane, no terminal in the loop{r}\n\n"
    ));
    s.push_str(&format!(
        "Every CLI subcommand mirrors an RPC method 1:1 — RPC dots become CLI \
         spaces ({mono}session.create{r} → {mono}shux session create{r}). Drive raw \
         RPCs directly via {mono}`shux rpc call <method> --params @file`{r} \
         (also accepts {mono}-{r} for stdin and inline JSON).",
    ));
    s
}

pub fn agent_help() -> String {
    use std::io::IsTerminal;
    let colorize = std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal();
    render_agent_help(colorize)
}

pub fn render_agent_help(colorize: bool) -> String {
    // Brand palette in 24-bit truecolor — matches the landing page.
    let acc = if colorize {
        "\x1b[1;38;2;215;108;58m"
    } else {
        ""
    }; // bold terracotta — section headers, `shux` brand
    let acc_dim = if colorize { "\x1b[38;2;199;90;42m" } else { "" }; // terracotta — `shux <verb>` ledes & URLs
    let cmd = if colorize {
        "\x1b[1;38;2;215;108;58m"
    } else {
        ""
    }; // bold terracotta — `shux` token
    let verb = if colorize { "\x1b[1;32m" } else { "" }; // bold green — subcommand verb
    let rpc = if colorize { "\x1b[32m" } else { "" }; // green — RPC method names
    let arrow = if colorize {
        "\x1b[38;2;146;138;120m"
    } else {
        ""
    }; // muted warm gray — →
    let dim = if colorize { "\x1b[2m" } else { "" }; // dim — inline comments
    let underline = if colorize { "\x1b[4m" } else { "" }; // underline — URLs
    let r = if colorize { "\x1b[0m" } else { "" }; // reset

    // Helper to render a `shux <verb>` token in two-tone colour.
    let shux = |v: &str| format!("{cmd}shux{r} {verb}{v}{r}");
    // Helper to render a section header.
    let h = |s: &str| format!("{acc}{s}{r}");
    // Helper to render an RPC method name.
    let m = |s: &str| format!("{rpc}{s}{r}");
    // Helper for arrows.
    let a = format!("{arrow}→{r}");
    // Helper for `shux` brand-name only.
    let sx = format!("{cmd}shux{r}");

    let mut s = String::with_capacity(4096);
    s.push_str(&format!("{}\n", h("COMMAND → RPC METHOD MAP")));
    s.push_str(&format!(
        "  {dim}RPC dots become CLI spaces. Every noun is namespaced.{r}\n\n"
    ));
    s.push_str(&format!(
        "  {:24} {a} {}\n",
        shux("session create"),
        m("session.create")
    ));
    s.push_str(&format!(
        "  {:24} {a} {}\n",
        shux("session list"),
        m("session.list")
    ));
    s.push_str(&format!(
        "  {:24} {a} {}\n",
        shux("session kill"),
        m("session.kill")
    ));
    s.push_str(&format!(
        "  {:24} {a} {}\n",
        shux("session rename"),
        m("session.rename")
    ));
    s.push_str(&format!(
        "  {:24} {a} {} {dim}(client-side, not RPC){r}\n",
        shux("session attach"),
        m("(attach)")
    ));
    s.push_str(&format!(
        "  {:24} {a} {}\n",
        shux("window <verb>"),
        m("window.{create,list,focus,kill,rename,reorder,ensure,snapshot}")
    ));
    s.push_str(&format!("  {:24} {a} {}\n", shux("pane <verb>"), m("pane.{send-keys,set-size,snapshot,capture,split,focus,zoom,swap,kill,set-title,resize,wait-for,output.watch,run}")));
    s.push_str(&format!(
        "  {:24} {a} {}\n",
        shux("plugin <verb>"),
        m("plugin.{install,list,kill,reload}")
    ));
    s.push_str(&format!(
        "  {:24} {a} {} / {}\n",
        shux("events <verb>"),
        m("events.history"),
        m("events.watch")
    ));
    s.push_str(&format!(
        "  {:24} {a} {} {dim}(atomic batch from a TOML template){r}\n",
        shux("state apply"),
        m("state.apply")
    ));
    s.push_str(&format!(
        "  {:24} {a} any method directly  {dim}(`--params @file` / `-` / inline){r}\n\n",
        shux("rpc call")
    ));

    // Issue #120: every listing prints ids truncated to 8 characters, and
    // for a long time nothing accepted them back. Say the rule once, here,
    // where a reader meets the ids for the first time.
    s.push_str(&format!(
        "{}\n",
        h("REFERRING TO SESSIONS, WINDOWS AND PANES")
    ));
    s.push_str(&format!(
        "  {dim}Lists print ids shortened to 8 characters, like git commit SHAs.{r}\n"
    ));
    s.push_str(&format!(
        "  {dim}Pass that short form back anywhere an id is wanted — or any\n  \
         unambiguous prefix of at least 4 characters, or the full uuid.{r}\n\n"
    ));
    s.push_str(&format!("  {} -s demo\n", shux("pane list")));
    s.push_str(&format!("  {dim}b57c601b  /home/you/project  nvim{r}\n"));
    s.push_str(&format!("  {} b57c601b\n", shux("pane glance")));
    s.push_str(&format!(
        "  {dim}Sessions and windows also answer to their name; an exact name\n  \
         wins over a partial id. A prefix two entities share is refused, and\n  \
         the error names them both.{r}\n\n"
    ));

    s.push_str(&format!("{}\n", h("TYPICAL AGENT WORKFLOW")));
    s.push_str(&format!(
        "  {dim}# 1. Spawn a session in the caller's cwd running any command.{r}\n"
    ));
    s.push_str(&format!(
        "  {} demo --title demo -- lazygit\n",
        shux("session create"),
    ));
    s.push_str(&format!(
        "  {dim}# Raw RPC callers should pass cwd explicitly.{r}\n"
    ));
    s.push_str(&format!(
        "  {} --params \"{{\\\"name\\\":\\\"demo\\\",\\\"cwd\\\":\\\"$(pwd)\\\",\\\"command\\\":[\\\"lazygit\\\"]}}\"\n\n",
        shux("rpc call session.create"),
    ));
    s.push_str(&format!(
        "  {dim}# 2. Drive it. (Synchronous resize — next snapshot sees new dims.){r}\n"
    ));
    s.push_str(&format!(
        "  {} -p b57c601b --cols 200 --rows 60\n",
        shux("pane set-size -s demo"),
    ));
    s.push_str(&format!(
        "  {} -s demo --text 'j'\n",
        shux("pane send-keys"),
    ));
    s.push_str(&format!(
        "  {} -s demo --data 'Gw=='   {dim}# Esc (base64){r}\n\n",
        shux("pane send-keys"),
    ));
    s.push_str(&format!(
        "  {dim}# 3. Pixel feedback (PNG, headless — no terminal emulator in the loop).{r}\n"
    ));
    s.push_str(&format!(
        "  {} --params '{{\"pane_id\":\"$PID\"}}' \\\n",
        shux("rpc call pane.snapshot"),
    ));
    s.push_str("    | jq -r .result.png_base64 | base64 -d > frame.png\n\n");
    s.push_str(&format!("  {dim}# Tear down when done.{r}\n"));
    s.push_str(&format!("  {} demo\n\n", shux("session kill")));

    s.push_str(&format!("{}\n", h("DECLARATIVE WORKSPACES")));
    s.push_str("  echo '[session]\n");
    s.push_str("  name=\"review\"\n");
    s.push_str("  [[windows]]\n");
    s.push_str("  title=\"git\"\n");
    s.push_str("  [[windows.panes]]\n");
    s.push_str("  command=[\"lazygit\"]' > spec.toml\n");
    s.push_str(&format!(
        "  {} spec.toml   {dim}# atomic; --dry-run prints the lowered ops{r}\n\n",
        shux("state apply"),
    ));

    s.push_str(&format!("{}\n", h("REPLACES THESE TOOLS")));
    let row = |tool: &str, with: &str| format!("  {tool:30} {a} {with}\n");
    s.push_str(&row(
        "tmux / screen / byobu",
        &format!("{} + {}", shux("state apply"), shux("session attach")),
    ));
    s.push_str(&row(
        "iTerm2 (Python SDK / AS)",
        &format!("{} + {}", m("pane.send_keys"), m("pane.snapshot")),
    ));
    s.push_str(&row(
        "expect / pexpect / sexpect",
        &format!(
            "{} {a} {} {a} {}",
            m("pane.send_keys"),
            m("pane.wait_for"),
            m("pane.capture")
        ),
    ));
    s.push_str(&row(
        "iTerm2 wait_for_text / _absent",
        &format!("{} {dim}(text · regex · --absent){r}", m("pane.wait_for")),
    ));
    s.push_str(&row(
        "asciinema rec / script(1)",
        &format!("{} {dim}(lossless raw PTY bytes){r}", m("pane.record")),
    ));
    s.push_str(&row(
        "vhs / agg / terminalizer",
        &format!("{} loop {a} ffmpeg", m("window.snapshot")),
    ));
    s.push_str(&row(
        "termshot / freezeframe",
        &format!("{} {dim}or{r} {}", m("pane.snapshot"), m("window.snapshot")),
    ));
    s.push_str(&row(
        "iTerm2 broadcast input",
        &format!("{} fan-out", m("pane.send_keys")),
    ));
    s.push_str(&row(
        "ttyrec / termsh",
        &format!("re-feed VT bytes {a} {}", m("pane.snapshot")),
    ));
    s.push_str(&row(
        "GNU parallel --tmux mode",
        "template with N panes + RPC orchestrator",
    ));
    s.push_str(&row(
        "Bubbletea / ratatui test harness",
        &format!("{} + golden-image diff", m("window.snapshot")),
    ));
    s.push('\n');

    let url = |u: &str| format!("{acc_dim}{underline}{u}{r}");
    s.push_str(&format!("{}\n", h("WHERE TO LEARN MORE")));
    s.push_str(&format!(
        "  Landing & live demos     {}\n",
        url("https://shux.pages.dev")
    ));
    s.push_str(&format!(
        "  Agent skill (drop-in)    {}\n",
        url("https://github.com/indrasvat/shux/tree/main/skills/shux")
    ));
    s.push_str(&format!(
        "  RPC reference            {}\n",
        url("https://github.com/indrasvat/shux/tree/main/skills/shux/references/api.md")
    ));
    s.push_str(&format!(
        "  Repository               {}\n\n",
        url("https://github.com/indrasvat/shux")
    ));

    s.push_str(&format!(
        "  Every entity in {sx} carries a 'version' field — pass 'expected_version' on\n"
    ));
    s.push_str(&format!(
        "  mutating RPCs for optimistic-concurrency rejection ({rpc}-32002{r}) on stale writes."
    ));

    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_help_raw_rpc_cwd_example_is_copy_safe() {
        let help = render_agent_help(false);

        assert!(
            help.contains(r#"--params "{\"name\":\"demo\",\"cwd\":\"$(pwd)\","#),
            "raw RPC cwd example should use shell-expanded $(pwd) in double-quoted JSON"
        );
        assert!(
            !help.contains(r#""cwd":"$PWD""#),
            "single-quoted inline JSON would send literal $PWD"
        );
    }
}
