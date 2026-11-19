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

    // Re-run when HEAD moves. Asked of git, not spelled `../../.git/HEAD`: in a
    // worktree `.git` is a file, and a path that never exists reruns this script
    // (and rebuilds every dependent crate) on every build.
    println!("cargo:rerun-if-changed=build.rs");
    let paths = git(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "HEAD",
        "--git-path",
        "refs",
        "--git-path",
        "packed-refs",
    ]);
    for path in paths.iter().flat_map(|p| p.lines()) {
        if std::path::Path::new(path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}
