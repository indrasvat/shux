use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
}

fn main() {
    // Capture git short SHA at compile time
    let sha = git(&["rev-parse", "--short", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=SHUX_GIT_SHA={sha}");

    // Re-run when HEAD moves: HEAD itself, the one ref it names, and
    // packed-refs. Asked of git, not spelled `../../.git/HEAD`: in a worktree
    // `.git` is a file, and a path that never exists reruns this script (and
    // rebuilds every dependent crate) on every build. Not all of `refs/`,
    // which every worktree's commits touch.
    println!("cargo:rerun-if-changed=build.rs");
    let branch = git(&["symbolic-ref", "-q", "HEAD"]);
    let mut args = vec!["rev-parse", "--path-format=absolute", "--git-path", "HEAD"];
    if let Some(branch) = branch.as_deref().map(str::trim) {
        args.extend(["--git-path", branch]);
    }
    args.extend(["--git-path", "packed-refs"]);
    for path in git(&args).iter().flat_map(|p| p.lines()) {
        if std::path::Path::new(path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}
