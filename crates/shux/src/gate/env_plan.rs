//! Deterministic env plan + `cmd_env_hash` / `scenario_hash` (design D4/D5). Pure.
//!
//! The plan is delivered to `lens.run` with `env_clear=true` (deny-by-default): the
//! child starts from an EMPTY env, then the runner's full plan is applied. Defaults →
//! allow-listed host passthrough → scenario `[env]` (later wins). NO host var leaks
//! except an explicit `allow`.
//!
//! `cmd_env_hash` is release-STABLE and run-STABLE: volatile sandbox paths (`HOME`,
//! `TMPDIR`, `XDG_*`, socket) are normalized by KEY IDENTITY (a runner-set sandbox value
//! → a structural `sandbox` marker, so two runs hash the same and a literal `"<sandbox>"`
//! override cannot collide); version-bearing daemon infra (`TERM_PROGRAM_VERSION`) is
//! never in the plan, so a shux release never churns it (design D5). The hash input is
//! canonical JSON (escaped), so a delimiter injected into an env value or argv element
//! cannot forge a collision. `scenario_hash` covers the scenario STRUCTURE, env values
//! excluded.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;
use shux_vt::sha256_hex;

use super::scenario::{Scenario, TerminalCfg};

/// The per-scenario sandbox directories (created by the runner as temp dirs). Their
/// VALUES are volatile per-run; the plan sets them so the child cannot touch host
/// state, and `cmd_env_hash` normalizes them to a sentinel.
#[derive(Debug, Clone)]
pub struct SandboxDirs {
    pub home: PathBuf,
    pub tmpdir: PathBuf,
    pub xdg_config: PathBuf,
    pub xdg_state: PathBuf,
    pub xdg_data: PathBuf,
    pub xdg_cache: PathBuf,
    pub xdg_runtime: PathBuf,
    pub shux_socket: PathBuf,
}

impl SandboxDirs {
    /// The (key, path) pairs the runner injects for isolation.
    fn entries(&self) -> [(&'static str, &PathBuf); 8] {
        [
            ("HOME", &self.home),
            ("TMPDIR", &self.tmpdir),
            ("XDG_CONFIG_HOME", &self.xdg_config),
            ("XDG_STATE_HOME", &self.xdg_state),
            ("XDG_DATA_HOME", &self.xdg_data),
            ("XDG_CACHE_HOME", &self.xdg_cache),
            ("XDG_RUNTIME_DIR", &self.xdg_runtime),
            ("SHUX_SOCKET", &self.shux_socket),
        ]
    }
}

/// A deterministic PATH used unless the scenario overrides or allow-lists it.
const DEFAULT_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// The resolved child environment (design D4). `env_clear` is always true for gate
/// runs; `env` is the full plan applied on top of the cleared environment.
#[derive(Debug, Clone, PartialEq)]
pub struct EnvPlan {
    pub env: BTreeMap<String, String>,
    pub env_clear: bool,
}

/// Build the deterministic env plan. `ambient` resolves a host var for allow-list
/// passthrough (injected in tests). `sandbox` supplies the isolation dirs.
pub fn build_env_plan(
    scenario: &Scenario,
    sandbox: &SandboxDirs,
    ambient: &dyn Fn(&str) -> Option<String>,
) -> EnvPlan {
    let mut env: BTreeMap<String, String> = BTreeMap::new();

    // 1. Deterministic defaults.
    for (k, v) in [
        ("LC_ALL", "C.UTF-8"),
        ("LANG", "C.UTF-8"),
        ("TZ", "UTC"),
        ("TERM", "xterm-256color"),
        ("COLORTERM", "truecolor"),
        ("SOURCE_DATE_EPOCH", "0"),
        ("PATH", DEFAULT_PATH),
    ] {
        env.insert(k.into(), v.into());
    }
    // Sandbox isolation (no host-temp / host-config / host-daemon reach).
    for (k, p) in sandbox.entries() {
        env.insert(k.into(), p.display().to_string());
    }

    // 2. Allow-listed host passthrough (opt-in escape hatch).
    for key in &scenario.env.allow {
        if let Some(v) = ambient(key) {
            env.insert(key.clone(), v);
        }
    }

    // 3. Scenario `[env]` — SETS (incl. empty string), wins over defaults/allow.
    for (k, v) in &scenario.env.vars {
        env.insert(k.clone(), v.clone());
    }

    EnvPlan {
        env,
        env_clear: true,
    }
}

/// A per-env-value repr for the `cmd_env_hash` structure. A runner-set volatile sandbox
/// path serializes as `{"sandbox":true}`; every other value as `{"value":"…"}` — so a
/// scenario that literally sets a var to the string `"<sandbox>"` can never collide with
/// the normalized sandbox marker (adv MINOR: a value-substitution sentinel could — this
/// normalizes by KEY IDENTITY instead).
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum EnvValRepr<'a> {
    Sandbox,
    Value(&'a str),
}

/// The canonical, injection-proof structure hashed for `cmd_env_hash`. Serializing to
/// JSON (escaped strings, a map, an argv array) means a `\n`/`=`/`\u{1f}` inside an env
/// value or argv element can NOT forge a collision the way a delimiter-joined string
/// could (adv MAJOR). `BTreeMap` gives a deterministic key order.
#[derive(Serialize)]
struct CmdEnvId<'a> {
    v: u32,
    argv: &'a [String],
    rows: u16,
    cols: u16,
    respond_to_queries: bool,
    env: BTreeMap<&'a str, EnvValRepr<'a>>,
}

/// Release- and run-stable identity of "what command in what environment produced this
/// frame" (design D5). Volatile sandbox paths normalize by key identity (so two runs
/// hash the same); scenario-overridden values are identity. Version-bearing daemon infra
/// is never in the plan, so it never appears here.
pub fn cmd_env_hash(
    plan: &EnvPlan,
    sandbox: &SandboxDirs,
    argv: &[String],
    terminal: &TerminalCfg,
) -> String {
    let sandbox_vals: BTreeMap<&str, String> = sandbox
        .entries()
        .iter()
        .map(|(k, p)| (*k, p.display().to_string()))
        .collect();

    let env: BTreeMap<&str, EnvValRepr> = plan
        .env
        .iter()
        .map(|(k, v)| {
            // Normalize ONLY a value the runner set to this key's sandbox path; a scenario
            // override (value ≠ sandbox path) stays identity.
            let repr = if sandbox_vals.get(k.as_str()) == Some(v) {
                EnvValRepr::Sandbox
            } else {
                EnvValRepr::Value(v.as_str())
            };
            (k.as_str(), repr)
        })
        .collect();

    let id = CmdEnvId {
        v: 1,
        argv,
        rows: terminal.rows,
        cols: terminal.cols,
        respond_to_queries: terminal.respond_to_queries,
        env,
    };
    sha256_hex(
        serde_json::to_string(&id)
            .expect("cmd_env id serializes")
            .as_bytes(),
    )
}

/// Canonical structure hashed for `scenario_hash` — env VALUES deliberately excluded
/// (env identity is `cmd_env_hash`'s job).
#[derive(Serialize)]
struct ScenarioStructure<'a> {
    name: &'a str,
    description: &'a str,
    command: &'a [String],
    /// Skipped when absent so every pre-084 scenario hashes exactly as before.
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<&'a str>,
    rows: u16,
    cols: u16,
    respond_to_queries: bool,
    deadline_ms: u64,
    steps: &'a [super::scenario::Step],
}

/// Identity of the scenario STRUCTURE (design D5): name/description/command/terminal/
/// steps. Env values are excluded (they live in `cmd_env_hash`).
pub fn scenario_hash(scenario: &Scenario) -> String {
    let st = ScenarioStructure {
        name: &scenario.name,
        description: &scenario.description,
        command: &scenario.command,
        cwd: scenario.cwd.as_deref(),
        rows: scenario.terminal.rows,
        cols: scenario.terminal.cols,
        respond_to_queries: scenario.terminal.respond_to_queries,
        deadline_ms: scenario.deadline_ms,
        steps: &scenario.steps,
    };
    let json = serde_json::to_string(&st).expect("scenario structure serializes");
    sha256_hex(json.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::super::scenario::parse;
    use super::*;

    fn sandbox(root: &str) -> SandboxDirs {
        let r = PathBuf::from(root);
        SandboxDirs {
            home: r.join("home"),
            tmpdir: r.join("tmp"),
            xdg_config: r.join("config"),
            xdg_state: r.join("state"),
            xdg_data: r.join("data"),
            xdg_cache: r.join("cache"),
            xdg_runtime: r.join("run"),
            shux_socket: r.join("run/shux.sock"),
        }
    }

    const SC: &str = r#"
name = "s"
command = ["/bin/echo", "hi"]
[terminal]
rows = 24
cols = 80
[env]
allow = ["EXTRA_TOOL"]
NO_COLOR = "1"
"#;

    #[test]
    fn plan_has_deterministic_defaults_and_sandbox() {
        let s = parse(SC).unwrap();
        let sb = sandbox("/sbx");
        let plan = build_env_plan(&s, &sb, &|_| None);
        assert!(plan.env_clear);
        assert_eq!(plan.env.get("LC_ALL").map(String::as_str), Some("C.UTF-8"));
        assert_eq!(plan.env.get("TZ").map(String::as_str), Some("UTC"));
        assert_eq!(
            plan.env.get("TERM").map(String::as_str),
            Some("xterm-256color")
        );
        assert_eq!(
            plan.env.get("COLORTERM").map(String::as_str),
            Some("truecolor")
        );
        assert_eq!(
            plan.env.get("SOURCE_DATE_EPOCH").map(String::as_str),
            Some("0")
        );
        assert_eq!(plan.env.get("HOME").map(String::as_str), Some("/sbx/home"));
        assert_eq!(plan.env.get("TMPDIR").map(String::as_str), Some("/sbx/tmp"));
        assert_eq!(
            plan.env.get("XDG_RUNTIME_DIR").map(String::as_str),
            Some("/sbx/run")
        );
        assert_eq!(
            plan.env.get("SHUX_SOCKET").map(String::as_str),
            Some("/sbx/run/shux.sock")
        );
        // Scenario [env] wins.
        assert_eq!(plan.env.get("NO_COLOR").map(String::as_str), Some("1"));
    }

    #[test]
    fn allow_list_passes_host_var_but_deny_by_default() {
        let s = parse(SC).unwrap();
        let sb = sandbox("/sbx");
        let ambient = |k: &str| match k {
            "EXTRA_TOOL" => Some("/opt/tool".to_string()),
            "SECRET" => Some("leak".to_string()),
            _ => None,
        };
        let plan = build_env_plan(&s, &sb, &ambient);
        // Allowed host var passes through.
        assert_eq!(
            plan.env.get("EXTRA_TOOL").map(String::as_str),
            Some("/opt/tool")
        );
        // A non-allowed host var does NOT leak.
        assert!(!plan.env.contains_key("SECRET"));
    }

    #[test]
    fn no_color_default_unset() {
        let s = parse("name=\"s\"\ncommand=[\"true\"]\n").unwrap();
        let plan = build_env_plan(&s, &sandbox("/sbx"), &|_| None);
        assert!(!plan.env.contains_key("NO_COLOR"), "color on by default");
    }

    #[test]
    fn cmd_env_hash_is_stable_across_sandbox_paths() {
        // Two different sandbox roots → SAME cmd_env_hash (volatile paths normalized).
        let s = parse(SC).unwrap();
        let a = build_env_plan(&s, &sandbox("/run-A"), &|_| None);
        let b = build_env_plan(&s, &sandbox("/run-B-different"), &|_| None);
        let ha = cmd_env_hash(&a, &sandbox("/run-A"), &s.command, &s.terminal);
        let hb = cmd_env_hash(&b, &sandbox("/run-B-different"), &s.command, &s.terminal);
        assert_eq!(ha, hb, "sandbox path churn must not change cmd_env_hash");
    }

    #[test]
    fn cmd_env_hash_reflects_determinism_vars_and_argv() {
        let s = parse(SC).unwrap();
        let sb = sandbox("/sbx");
        let plan = build_env_plan(&s, &sb, &|_| None);
        let base = cmd_env_hash(&plan, &sb, &s.command, &s.terminal);
        // A different argv → different hash.
        let other = cmd_env_hash(&plan, &sb, &["/bin/echo".into(), "BYE".into()], &s.terminal);
        assert_ne!(base, other);
        // A different geometry → different hash.
        let mut term = s.terminal.clone();
        term.cols = 120;
        assert_ne!(base, cmd_env_hash(&plan, &sb, &s.command, &term));
    }

    #[test]
    fn cmd_env_hash_scenario_override_of_sandbox_key_is_identity() {
        // If a scenario explicitly sets HOME, it is identity (not normalized away).
        let s1 = parse("name=\"s\"\ncommand=[\"true\"]\n").unwrap();
        let s2 = parse("name=\"s\"\ncommand=[\"true\"]\n[env]\nHOME=\"/fixed\"\n").unwrap();
        let sb = sandbox("/sbx");
        let p1 = build_env_plan(&s1, &sb, &|_| None);
        let p2 = build_env_plan(&s2, &sb, &|_| None);
        let h1 = cmd_env_hash(&p1, &sb, &s1.command, &s1.terminal);
        let h2 = cmd_env_hash(&p2, &sb, &s2.command, &s2.terminal);
        assert_ne!(h1, h2, "an explicit HOME override is part of identity");
    }

    #[test]
    fn cmd_env_hash_resists_delimiter_injection() {
        // adv MAJOR: a `\n`/`=` inside an env value must NOT forge a collision with two
        // separate vars. Structured JSON hashing makes the two plans distinct.
        let sb = sandbox("/sbx");
        let a = parse("name=\"s\"\ncommand=[\"true\"]\n[env]\nK=\"v1\\nK2=v2\"\n").unwrap();
        let b = parse("name=\"s\"\ncommand=[\"true\"]\n[env]\nK=\"v1\"\nK2=\"v2\"\n").unwrap();
        let pa = build_env_plan(&a, &sb, &|_| None);
        let pb = build_env_plan(&b, &sb, &|_| None);
        assert_ne!(pa.env, pb.env);
        assert_ne!(
            cmd_env_hash(&pa, &sb, &a.command, &a.terminal),
            cmd_env_hash(&pb, &sb, &b.command, &b.terminal),
            "delimiter injection must not collide"
        );
        // Same for argv (a `\u{1f}` inside an element cannot merge two elements).
        let base = cmd_env_hash(&pa, &sb, &["a".into(), "b".into()], &a.terminal);
        let inj = cmd_env_hash(&pa, &sb, &["a\u{1f}b".into()], &a.terminal);
        assert_ne!(base, inj);
    }

    #[test]
    fn cmd_env_hash_sentinel_literal_does_not_collide() {
        // adv MINOR: a scenario setting HOME to the literal "<sandbox>" must NOT hash the
        // same as the real (normalized) sandbox HOME.
        let sb = sandbox("/sbx");
        let default = parse("name=\"s\"\ncommand=[\"true\"]\n").unwrap();
        let literal = parse("name=\"s\"\ncommand=[\"true\"]\n[env]\nHOME=\"<sandbox>\"\n").unwrap();
        let pd = build_env_plan(&default, &sb, &|_| None);
        let pl = build_env_plan(&literal, &sb, &|_| None);
        assert_ne!(
            cmd_env_hash(&pd, &sb, &default.command, &default.terminal),
            cmd_env_hash(&pl, &sb, &literal.command, &literal.terminal),
            "a literal \"<sandbox>\" override must not collide with the normalized sandbox path"
        );
    }

    #[test]
    fn scenario_hash_ignores_env_values() {
        let a = parse("name=\"s\"\ncommand=[\"true\"]\n[env]\nX=\"1\"\n").unwrap();
        let b = parse("name=\"s\"\ncommand=[\"true\"]\n[env]\nX=\"2\"\n").unwrap();
        assert_eq!(
            scenario_hash(&a),
            scenario_hash(&b),
            "scenario_hash is structure-only; env values differ but structure is same"
        );
    }

    #[test]
    fn scenario_hash_changes_with_steps() {
        let a = parse("name=\"s\"\ncommand=[\"true\"]\n").unwrap();
        let b =
            parse("name=\"s\"\ncommand=[\"true\"]\n[[steps]]\naction=\"wait\"\nms=1\n").unwrap();
        assert_ne!(scenario_hash(&a), scenario_hash(&b));
    }
}
