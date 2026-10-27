#!/usr/bin/env bash
# Keep Cargo build dirs from growing without bound, and empty them safely.
#
# On macOS cargo's default split-debuginfo is "unpacked": every codegen unit's
# .o stays in target/**/deps and stale ones are never pruned. A long-lived
# checkout reached ~900k files there, and every build, `nextest list`, `ls` and
# first exec of a test binary then stalled for minutes.
#
#   check-target-hygiene.sh [repo-root]           check (read-only)
#   check-target-hygiene.sh --clean [repo-root]   empty the repo's build dirs
#
# check, for every Cargo workspace root in the repo:
#   1. the manifest pins split-debuginfo = "packed" for every profile that
#      builds with debug info, through `inherits`, package and build overrides
#      (portable: CI and other machines read only the manifest);
#   2. the settings cargo will really use here (manifest, then .cargo/config.toml
#      layers, then CARGO_PROFILE_* env) resolve to "packed" as well;
#   3. no target dir holds more than STALE_LIMIT Rust objects older than a
#      build could still be writing (any at all are warned about). Reported,
#      never deleted: age cannot prove an object is garbage (a slow link reads
#      its inputs late). `make clean` fixes it.
#
# --clean empties only <workspace>/target dirs cargo creates by default, plus
# the test-inventory cache: real directories inside the repo, never a symlink
# and never a configured CARGO_TARGET_DIR. Anything else is named, not touched.
set -euo pipefail

mode=check
if [[ "${1:-}" == "--clean" ]]; then
  mode=clean
  shift
fi
ROOT="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"

# -c, not a heredoc on stdin: the guard never needs stdin, but keep the habit.
PROGRAM="$(cat <<'PY'
import json
import os
import re
import shutil
import subprocess
import sys
import time
import tomllib

mode, root = sys.argv[1], os.path.realpath(sys.argv[2])
STALE_SECONDS = 60 * int(os.environ.get("STALE_MINUTES", "10"))
# A SIGKILLed rustc leaves its in-flight objects whatever the profile says, a
# few hundred per kill. Harmless until they pile up; ~900k is what stalled
# this machine. Warn on any, fail past the limit.
STALE_LIMIT = int(os.environ.get("STALE_LIMIT", "2000"))
SKIP_DIRS = {".git", "target", "node_modules"}
SCRATCH = os.path.join(root, ".shux", "out")
failures = []


def fail(msg):
    failures.append(msg)


def on_walk_error(e):
    fail(f"cannot read {e.filename}: {e.strerror}")


def load_toml(path):
    try:
        with open(path, "rb") as fh:
            return tomllib.load(fh)
    except FileNotFoundError:
        return None
    except (OSError, tomllib.TOMLDecodeError) as exc:
        fail(f"{path}: {exc}")
        return None


def workspace_roots():
    roots = []
    for dirpath, dirnames, filenames in os.walk(root, onerror=on_walk_error):
        dirnames[:] = sorted(d for d in dirnames
                             if d not in SKIP_DIRS and os.path.join(dirpath, d) != SCRATCH)
        if "Cargo.toml" in filenames:
            path = os.path.join(dirpath, "Cargo.toml")
            manifest = load_toml(path)
            if manifest is not None and "workspace" in manifest:
                roots.append((path, manifest))
    return roots


# ── Profile resolution ─────────────────────────────────────────────────────
BUILTIN_PARENT = {"test": "dev", "bench": "release"}
BUILTIN_DEBUG = {"dev": True, "release": False}


def merge(base, over):
    out = dict(base)
    for key, value in over.items():
        if isinstance(value, dict) and isinstance(out.get(key), dict):
            out[key] = merge(out[key], value)
        elif isinstance(value, list) and isinstance(out.get(key), list):
            out[key] = out[key] + value  # cargo concatenates arrays (rustflags)
        else:
            out[key] = value
    return out


def config_file(directory):
    """Cargo reads `config` in preference to `config.toml` when both exist."""
    for name in ("config", "config.toml"):
        path = os.path.join(directory, name)
        if os.path.isfile(path):
            return path
    return None


def load_config(path, seen=()):
    """A cargo config file with its `include`s merged under it (relative to
    the including file; `optional` ones may be missing)."""
    if path in seen:
        fail(f"{path}: config include cycle")
        return {}
    table = load_toml(path) or {}
    merged = {}
    includes = table.get("include", [])
    for inc in [includes] if isinstance(includes, (str, dict)) else includes:
        rel, optional = (inc.get("path"), inc.get("optional", False)) if isinstance(inc, dict) else (inc, False)
        target = os.path.join(os.path.dirname(path), str(rel))
        if os.path.isfile(target):
            merged = merge(merged, load_config(target, seen + (path,)))
        elif not optional:
            fail(f"{path}: included config {target} does not exist")
    return merge(merged, {k: v for k, v in table.items() if k != "include"})


def cargo_config(workspace_dir):
    """The cargo config that applies to a build started in workspace_dir:
    $CARGO_HOME first, then each ancestor's .cargo/, nearest last."""
    dirs = [os.environ.get("CARGO_HOME", os.path.expanduser("~/.cargo"))]
    chain, d = [], workspace_dir
    while True:
        chain.append(os.path.join(d, ".cargo"))
        parent = os.path.dirname(d)
        if parent == d:
            break
        d = parent
    dirs.extend(reversed(chain))
    config = {}
    for directory in dirs:
        path = config_file(directory)
        if path:
            config = merge(config, load_config(path))
    return config


SPLIT_FLAG = re.compile(r"split[-_]debuginfo=([^\s\x1f\"',\]]+)")


def check_rustflags(label, config):
    """A -C split-debuginfo in rustflags wins over the profile."""
    sources = [(k, v) for k, v in os.environ.items()
               if k in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_RUSTFLAGS")
               or (k.startswith("CARGO_TARGET_") and k.endswith("_RUSTFLAGS"))]
    sources.append(("build.rustflags", config.get("build", {}).get("rustflags")))
    for triple, table in config.get("target", {}).items():
        if isinstance(table, dict):
            sources.append((f"target.{triple}.rustflags", table.get("rustflags")))
    for where, flags in sources:
        text = " ".join(flags) if isinstance(flags, list) else str(flags or "")
        for value in SPLIT_FLAG.findall(text):
            if value != "packed":
                fail(f"{label}: {where} passes -C split-debuginfo={value}, which overrides the profile")


def env_profiles():
    """CARGO_PROFILE_<NAME>_[BUILD_OVERRIDE_]{SPLIT_DEBUGINFO,DEBUG}. A package
    override cannot be parsed back from its env name (package names may hold
    `_`), so any *SPLIT_DEBUGINFO other than "packed" fails outright."""
    profiles = {}
    for key, value in os.environ.items():
        if not key.startswith("CARGO_PROFILE_"):
            continue
        if key.endswith("SPLIT_DEBUGINFO") and value != "packed":
            fail(f"environment: {key}={value!r}; split-debuginfo must be \"packed\"")
        rest = key[len("CARGO_PROFILE_"):]
        for setting in ("SPLIT_DEBUGINFO", "DEBUG"):
            if not rest.endswith("_" + setting):
                continue
            head, section = rest[: -len(setting) - 1], None
            if head.endswith("_BUILD_OVERRIDE"):
                head, section = head[: -len("_BUILD_OVERRIDE")], "build-override"
            elif "_PACKAGE_" in head:
                break
            table = profiles.setdefault(head.lower().replace("_", "-"), {})
            if section:
                table = table.setdefault(section, {})
            table[setting.lower().replace("_", "-")] = value
            break
    return profiles


def chain_of(profiles, name):
    chain, seen = [], set()
    while name and name not in seen:
        seen.add(name)
        chain.append(name)
        name = profiles.get(name, {}).get("inherits", BUILTIN_PARENT.get(name))
    return chain


def resolved(profiles, name):
    """Profile `name` with everything it inherits merged in, child winning."""
    table = {}
    for p in reversed(chain_of(profiles, name)):
        table = merge(table, profiles.get(p, {}))
    return table


def contexts(profiles, name):
    """(what, debug, split) for every way profile `name` compiles a crate, by
    cargo's precedence: named package, then "*", then build-override (build
    scripts and proc-macros only), then the profile. build-override's own
    debug default is off."""
    p = resolved(profiles, name)
    packages = p.get("package", {})
    override = p.get("build-override", {})
    base_debug = p.get("debug", BUILTIN_DEBUG.get(chain_of(profiles, name)[-1], True))
    base_split = p.get("split-debuginfo")
    out = []
    variants = [(None, [])]
    for pkg in sorted(packages):
        if pkg == "*":
            variants.append(("*", [packages["*"]]))
            continue
        # "*" covers non-members only; whether pkg is one is not known here, so
        # both readings are checked.
        variants.append((pkg, [packages[pkg]]))
        if "*" in packages:
            variants.append((pkg, [packages[pkg], packages["*"]]))
    for pkg, layers in variants:
        for build in (False, True):
            stack = layers + ([override] if build else [])
            debug = next((t["debug"] for t in stack if "debug" in t), False if build else base_debug)
            split = next((t["split-debuginfo"] for t in stack if "split-debuginfo" in t), base_split)
            what = f"profile '{name}'" + (f" package {pkg!r}" if pkg else "") + (" build-override" if build else "")
            out.append((what, debug, split))
    return out


def debug_on(value):
    return str(value).lower() not in ("none", "false", "0", "")


def check_profiles(label, profiles):
    for name in sorted({"dev", "release", "test", "bench", *profiles}):
        for what, debug, split in contexts(profiles, name):
            if debug_on(debug) and split != "packed":
                fail(f"{label}: {what} builds with debug info but split-debuginfo "
                     f"resolves to {split!r}; set it to \"packed\"")


# ── Target dirs ────────────────────────────────────────────────────────────
def metadata_dirs(path):
    """Cargo's target and (separate) build dirs for the workspace at path, as a
    build started there would resolve them."""
    proc = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--manifest-path", path],
        capture_output=True, text=True, cwd=os.path.dirname(path),
    )
    if proc.returncode != 0:
        fail(f"`cargo metadata` failed for {path}: {proc.stderr.strip().splitlines()[-1:]}")
        return []
    meta = json.loads(proc.stdout)
    return [d for d in (meta.get("target_directory"), meta.get("build_directory")) if d]


def stale_objects(target):
    """Count Rust codegen objects (*.rcgu.o) anywhere, and any .o directly in a
    deps dir, older than a build could still be writing. Native objects a
    build script compiles into its out/ are left alone."""
    now = time.time()
    total = 0
    for dirpath, dirnames, filenames in os.walk(target, onerror=on_walk_error):
        keep = []
        for d in dirnames:
            full = os.path.join(dirpath, d)
            if d.endswith(".dSYM") or d == "incremental":
                continue
            if os.path.islink(full):
                fail(f"{full} is a directory symlink inside a target dir; objects behind it "
                     f"cannot be checked")
                continue
            keep.append(d)
        dirnames[:] = keep
        in_deps = os.path.basename(dirpath) == "deps"
        stale = 0
        for name in filenames:
            if name.endswith(".rcgu.o") or (in_deps and name.endswith(".o")):
                try:
                    if now - os.lstat(os.path.join(dirpath, name)).st_mtime > STALE_SECONDS:
                        stale += 1
                except FileNotFoundError:
                    pass
        total += stale
    if total > STALE_LIMIT:
        fail(f"{total} stale .o file(s) under {target} (limit {STALE_LIMIT}); a build without "
             f"split-debuginfo = \"packed\" or killed rustc runs left them. Run: make clean")
    elif total:
        print(f"⚠ target hygiene: {total} stale .o file(s) under {target}, left by killed "
              f"builds; harmless below {STALE_LIMIT}, `make clean` when convenient",
              file=sys.stderr)


def empty_dir(path):
    """Delete everything inside path without following symlinks; True if it
    ends up empty."""
    def on_error(_func, failed, exc_info):
        fail(f"cannot delete {failed}: {exc_info[1]}")

    try:
        entries = os.listdir(path)
    except OSError as exc:
        fail(f"cannot read {path}: {exc.strerror}")
        return False
    for name in entries:
        full = os.path.join(path, name)
        try:
            if os.path.isdir(full) and not os.path.islink(full):
                shutil.rmtree(full, onerror=on_error)
            else:
                os.unlink(full)
        except OSError as exc:
            fail(f"cannot delete {full}: {exc.strerror}")
    try:
        left = os.listdir(path)
    except OSError as exc:
        fail(f"cannot read {path}: {exc.strerror}")
        return False
    if left:
        fail(f"{path} still holds {len(left)} entr{'y' if len(left) == 1 else 'ies'} after clean")
        return False
    return True


# ── Main ───────────────────────────────────────────────────────────────────
roots = workspace_roots()
if not roots and not failures:
    fail(f"no Cargo workspace root found under {root}")

approved = []      # dirs --clean may empty
scanned = []       # dirs check reads
env = env_profiles()
for path, manifest in roots:
    wdir = os.path.dirname(path)
    rel = os.path.relpath(path, root)
    manifest_profiles = manifest.get("profile", {})
    if mode == "check":
        check_profiles(f"{rel} (manifest)", manifest_profiles)
        config = cargo_config(wdir)
        used = merge(merge(manifest_profiles, config.get("profile", {})), env)
        check_rustflags(rel, config)
        check_profiles(f"{rel} (as built here, with cargo config and CARGO_PROFILE_* env)", used)
    approved.append(os.path.join(wdir, "target"))
    for t in (os.path.join(wdir, "target"), *metadata_dirs(path)):
        if os.path.realpath(t) not in scanned:
            scanned.append(os.path.realpath(t))
approved.append(os.path.join(root, ".shux", "out", "test-inventory", "target"))
inventory = os.path.realpath(approved[-1])
if os.path.isdir(inventory) and inventory not in scanned:
    scanned.append(inventory)

if mode == "clean":
    emptied = 0
    for t in approved:
        if not os.path.lexists(t):
            continue
        real = os.path.realpath(t)
        if os.path.islink(t) or real != t or not real.startswith(root + os.sep):
            print(f"⚠ target hygiene: not cleaning {t}: it is a symlink or resolves outside "
                  f"the repo ({real}); empty it yourself", file=sys.stderr)
            continue
        if not os.path.isdir(t):
            fail(f"{t} is not a directory")
            continue
        emptied += empty_dir(t)
    for t in scanned:
        if t not in (os.path.realpath(a) for a in approved):
            print(f"⚠ target hygiene: not cleaning configured target dir {t}; "
                  f"run `cargo clean` there if you own it", file=sys.stderr)
    if failures:
        for f in failures:
            print(f"✗ target hygiene: {f}", file=sys.stderr)
        sys.exit(1)
    print(f"✓ target hygiene: emptied {emptied} build dir(s)")
    sys.exit(0)

for t in scanned:
    if os.path.isdir(t):
        stale_objects(t)

if failures:
    for f in failures:
        print(f"✗ target hygiene: {f}", file=sys.stderr)
    sys.exit(1)
print(f"✓ target hygiene: {len(roots)} workspace(s) packed, "
      f"{len(scanned)} target dir(s) within the stale-object limit")
PY
)"
exec python3 -c "${PROGRAM}" "${mode}" "${ROOT}"
