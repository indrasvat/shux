#!/usr/bin/env bash
# Prove check-target-hygiene.sh fails on each defect it exists to catch, for
# the right reason, on missing input, and that --clean empties only the build
# dirs it owns.
set -euo pipefail

SCRIPTS="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GUARD="${HYGIENE_GUARD:-${SCRIPTS}/check-target-hygiene.sh}"
work="$(cd "$(mktemp -d)" && pwd -P)"
trap 'chmod -R u+rwx "${work}" 2>/dev/null; find "${work}" -delete' EXIT
# Isolated from this machine's cargo config and env, so a fixture's manifest is
# the only thing deciding the profile.
unset CARGO_TARGET_DIR
while IFS= read -r var; do unset "${var}"; done < <(env | sed -n 's/^\(CARGO_PROFILE_[A-Z0-9_]*\)=.*/\1/p')
export CARGO_HOME="${work}/cargo-home"
mkdir -p "${CARGO_HOME}"
export STALE_LIMIT=0

PACKED='[workspace]\n\n[profile.dev]\ndebug = "line-tables-only"\nsplit-debuginfo = "packed"\n\n[profile.test]\nopt-level = 0\n'

# A repo with a root workspace and a second, independent one (like fuzz/).
tree() {
  mkdir -p "$1/fuzz"
  printf '%b' "${PACKED}" >"$1/Cargo.toml"
  printf '[workspace]\n\n[profile.dev]\nsplit-debuginfo = "packed"\n\n[profile.release]\ndebug = 1\nsplit-debuginfo = "packed"\n' >"$1/fuzz/Cargo.toml"
}

stale() { mkdir -p "$(dirname "$1")"; touch -t 202001010000 "$1"; }

die() { echo "✗ target hygiene self-test: $*" >&2; exit 1; }

# expect pass|fail <name> <root> [diagnostic substring] — runs the read-only check.
expect() {
  local want="$1" name="$2" root="$3" needle="${4:-}" rc=0 out
  out="$("${GUARD}" "${root}" 2>&1)" || rc=$?
  if [[ "${want}" == pass && "${rc}" -ne 0 ]] || [[ "${want}" == fail && "${rc}" -eq 0 ]]; then
    die "'${name}' should ${want} (exit ${rc}):"$'\n'"${out}"
  fi
  if [[ -n "${needle}" && "${out}" != *"${needle}"* ]]; then
    die "'${name}' ${want}ed for the wrong reason (wanted '${needle}'):"$'\n'"${out}"
  fi
}

# ── Profiles ───────────────────────────────────────────────────────────────
t="${work}/clean"; tree "$t"
stale "$t/target/debug/deps/libfoo-abc.rlib"
stale "$t/target/debug/build/cc-abc/out/foo.o"
expect pass "packed profiles, no stale objects" "$t"

t="${work}/never"; tree "$t"
expect pass "never built" "$t"

t="${work}/spelling"; tree "$t"
printf "[workspace]\n\n[profile.dev] # comment\n  split-debuginfo = 'packed'\n" >"$t/Cargo.toml"
expect pass "comment, indentation and single quotes" "$t"

t="${work}/nodebug"; tree "$t"
printf '[workspace]\n\n[profile.dev]\ndebug = false\n' >"$t/Cargo.toml"
expect pass "dev without debug info keeps no objects" "$t"

t="${work}/unpacked"; tree "$t"
printf '[workspace]\n\n[profile.dev]\ndebug = "line-tables-only"\n' >"$t/Cargo.toml"
expect fail "dev without split-debuginfo" "$t" "Cargo.toml (manifest): profile 'dev'"

t="${work}/indented"; tree "$t"
printf '[workspace]\n\n[profile.dev]\ndebug = 1\n  [profile.dev.package."*"]\nsplit-debuginfo = "packed"\n' >"$t/Cargo.toml"
expect fail "packed only in an indented package override" "$t" "profile 'dev' builds with debug info"

t="${work}/release-only"; tree "$t"
printf '[workspace]\n\n[profile.release]\nsplit-debuginfo = "packed"\n' >"$t/Cargo.toml"
expect fail "packed on another profile only" "$t" "profile 'dev'"

t="${work}/test-override"; tree "$t"
printf '[workspace]\n\n[profile.dev]\nsplit-debuginfo = "packed"\n\n[profile.test]\nsplit-debuginfo = "unpacked"\n' >"$t/Cargo.toml"
expect fail "test profile overriding to unpacked" "$t" "profile 'test'"

t="${work}/custom"; tree "$t"
printf '[workspace]\n\n[profile.dev]\nsplit-debuginfo = "packed"\n\n[profile.ci]\ninherits = "release"\ndebug = true\n' >"$t/Cargo.toml"
expect fail "custom profile with debug info, unpacked" "$t" "profile 'ci'"

t="${work}/pkg"; tree "$t"
printf '[workspace]\n\n[profile.dev]\nsplit-debuginfo = "packed"\n\n[profile.dev.package.foo]\nsplit-debuginfo = "unpacked"\n' >"$t/Cargo.toml"
expect fail "package override to unpacked" "$t" "package 'foo'"

t="${work}/pkg-inherited"; tree "$t"
printf '[workspace]\n\n[profile.dev]\nsplit-debuginfo = "packed"\n\n[profile.release]\nsplit-debuginfo = "packed"\n\n[profile.release.package.dep]\ndebug = true\n\n[profile.custom]\ninherits = "release"\nsplit-debuginfo = "unpacked"\n' >"$t/Cargo.toml"
expect fail "package debug inherited by an unpacked custom profile" "$t" "profile 'custom' package 'dep'"

t="${work}/build-override"; tree "$t"
printf '[workspace]\n\n[profile.dev]\nsplit-debuginfo = "packed"\n\n[profile.release]\ndebug = false\n\n[profile.release.build-override]\ndebug = true\n' >"$t/Cargo.toml"
expect fail "build-override turning on debug info, unpacked" "$t" "profile 'release' build-override"

t="${work}/fuzz-dev"; tree "$t"
printf '[workspace]\n\n[profile.release]\nsplit-debuginfo = "packed"\n' >"$t/fuzz/Cargo.toml"
expect fail "second workspace with an unpacked dev" "$t" "fuzz/Cargo.toml (manifest): profile 'dev'"

t="${work}/env"; tree "$t"
CARGO_PROFILE_DEV_SPLIT_DEBUGINFO=unpacked expect fail "CARGO_PROFILE_* env undoing packed" "$t" "(as built here"

t="${work}/config"; tree "$t"; mkdir -p "$t/.cargo"
printf '[profile.dev]\nsplit-debuginfo = "unpacked"\n' >"$t/.cargo/config.toml"
expect fail ".cargo/config.toml undoing packed" "$t" "(as built here"

t="${work}/cargo-home-config"; tree "$t"; mkdir -p "${work}/home2"
printf '[profile.dev]\nsplit-debuginfo = "unpacked"\n' >"${work}/home2/config.toml"
CARGO_HOME="${work}/home2" expect fail "\$CARGO_HOME/config.toml undoing packed" "$t" "(as built here"

t="${work}/malformed"; tree "$t"; printf '[profile.dev\n' >"$t/fuzz/Cargo.toml"
expect fail "malformed manifest" "$t" "fuzz/Cargo.toml"

mkdir -p "${work}/empty"
expect fail "no workspace at all" "${work}/empty" "no Cargo workspace root"

t="${work}/legacy-config"; tree "$t"; mkdir -p "$t/.cargo"
printf '[profile.dev]\nsplit-debuginfo = "unpacked"\n' >"$t/.cargo/config"
expect fail "extensionless .cargo/config undoing packed" "$t" "(as built here"

t="${work}/combined"; tree "$t"
printf '[workspace]\n\n[profile.dev]\ndebug = false\nsplit-debuginfo = "packed"\n\n[profile.dev.build-override]\nsplit-debuginfo = "unpacked"\n\n[profile.dev.package.probe]\ndebug = true\n' >"$t/Cargo.toml"
expect fail "package debug + build-override split on one build script" "$t" "profile 'dev' package 'probe' build-override"

t="${work}/env-build-override"; tree "$t"
printf '[workspace]\n\n[profile.dev]\ndebug = false\nsplit-debuginfo = "packed"\n\n[profile.dev.build-override]\nsplit-debuginfo = "unpacked"\n' >"$t/Cargo.toml"
CARGO_PROFILE_DEV_BUILD_OVERRIDE_DEBUG=true expect fail "build-override debug from env" "$t" "(as built here, with cargo config and CARGO_PROFILE_* env): profile 'dev' build-override"

t="${work}/env-package"; tree "$t"
CARGO_PROFILE_DEV_PACKAGE_FOO_SPLIT_DEBUGINFO=unpacked expect fail "package split from env" "$t" "environment: CARGO_PROFILE_DEV_PACKAGE_FOO_SPLIT_DEBUGINFO"

t="${work}/member-wildcard"; tree "$t"
printf '[workspace]\n\n[profile.dev]\nsplit-debuginfo = "packed"\n\n[profile.dev.package."*"]\ndebug = false\n\n[profile.dev.package.memberprobe]\nsplit-debuginfo = "unpacked"\n' >"$t/Cargo.toml"
expect fail "named member override not covered by \"*\"" "$t" "profile 'dev' package 'memberprobe' builds"

t="${work}/rustflags-env"; tree "$t"
RUSTFLAGS="-Dwarnings -Csplit-debuginfo=unpacked" expect fail "RUSTFLAGS -Csplit-debuginfo=unpacked" "$t" "RUSTFLAGS passes -C split-debuginfo=unpacked"
RUSTFLAGS="-C split-debuginfo=off" expect fail "RUSTFLAGS -C split-debuginfo=off" "$t" "RUSTFLAGS passes -C split-debuginfo=off"
CARGO_ENCODED_RUSTFLAGS=$'-C\x1fsplit-debuginfo=unpacked' expect fail "CARGO_ENCODED_RUSTFLAGS" "$t" "CARGO_ENCODED_RUSTFLAGS passes"
RUSTFLAGS="-Dwarnings -Csplit-debuginfo=packed" expect pass "RUSTFLAGS that agree with the profile" "$t"
RUSTFLAGS="-C split_debuginfo=unpacked" expect fail "RUSTFLAGS underscore spelling" "$t" "RUSTFLAGS passes -C split-debuginfo=unpacked"

t="${work}/rustflags-config"; tree "$t"; mkdir -p "$t/.cargo"
printf '[build]\nrustflags = ["-C", "split-debuginfo=unpacked"]\n' >"$t/.cargo/config.toml"
expect fail "build.rustflags in cargo config" "$t" "build.rustflags passes -C split-debuginfo=unpacked"
printf "[target.aarch64-apple-darwin]\nrustflags = '-Csplit-debuginfo=unpacked'\n" >"$t/.cargo/config.toml"
expect fail "target.<triple>.rustflags in cargo config" "$t" "target.aarch64-apple-darwin.rustflags passes"

t="${work}/include"; tree "$t"; mkdir -p "$t/.cargo"
printf 'include = ["unsafe.toml"]\n' >"$t/.cargo/config.toml"
printf '[profile.dev]\nsplit-debuginfo = "unpacked"\n' >"$t/.cargo/unsafe.toml"
expect fail "a profile in an included config" "$t" "(as built here"
printf 'include = [{ path = "absent.toml", optional = true }]\n' >"$t/.cargo/config.toml"
expect pass "a missing optional include" "$t"
printf 'include = ["absent.toml"]\n' >"$t/.cargo/config.toml"
expect fail "a missing required include" "$t" "absent.toml does not exist"
printf 'include = ["config.toml"]\n' >"$t/.cargo/config.toml"
expect fail "an include cycle" "$t" "config include cycle"
printf 'include = ["extra.toml"]\n[build]\nrustflags = ["-Dwarnings"]\n' >"$t/.cargo/config.toml"
printf '[build]\nrustflags = ["-Csplit-debuginfo=unpacked"]\n' >"$t/.cargo/extra.toml"
expect fail "included rustflags concatenated with the includer's" "$t" "build.rustflags passes -C split-debuginfo=unpacked"

# ── Stale objects (report-only) ────────────────────────────────────────────
t="${work}/inflight"; tree "$t"; mkdir -p "$t/target/debug/deps"
touch "$t/target/debug/deps/foo.1a2b-cgu.0.rcgu.o"
expect pass "fresh .o of a build in progress" "$t"

t="${work}/below"; tree "$t"; stale "$t/target/debug/deps/a.rcgu.o"; stale "$t/target/debug/deps/b.rcgu.o"
STALE_LIMIT=5 expect pass "a few leftovers from killed builds warn" "$t" "2 stale .o file(s)"

check_stale() {
  expect fail "$1" "$2" "$3"
  [[ -e "$4" ]] || die "'$1': the read-only check deleted $4"
}
t="${work}/stale"; tree "$t"; stale "$t/target/debug/deps/foo.1a2b-cgu.0.rcgu.o"
check_stale "stale .o in target/debug/deps" "$t" "stale .o file(s) under $t/target" "$t/target/debug/deps/foo.1a2b-cgu.0.rcgu.o"

t="${work}/cross"; tree "$t"; stale "$t/target/aarch64-apple-darwin/debug/deps/x.rcgu.o"
check_stale "stale .o in a cross-target deps dir" "$t" "under $t/target" "$t/target/aarch64-apple-darwin/debug/deps/x.rcgu.o"

t="${work}/second"; tree "$t"; stale "$t/fuzz/target/release/deps/x.rcgu.o"
check_stale "stale .o in a second workspace" "$t" "under $t/fuzz/target" "$t/fuzz/target/release/deps/x.rcgu.o"

t="${work}/inventory"; tree "$t"; stale "$t/.shux/out/test-inventory/target/debug/deps/x.rcgu.o"
check_stale "stale .o in the test-inventory cache" "$t" "test-inventory/target" "$t/.shux/out/test-inventory/target/debug/deps/x.rcgu.o"

t="${work}/build-rcgu"; tree "$t"; stale "$t/target/debug/build/foo-abc/build_script_build-abc.foo.1a2b-cgu.0.rcgu.o"
check_stale "stale Rust codegen object in a build-script dir" "$t" "under $t/target" "$t/target/debug/build/foo-abc/build_script_build-abc.foo.1a2b-cgu.0.rcgu.o"

t="${work}/envtarget"; tree "$t"; stale "${work}/elsewhere/debug/deps/x.rcgu.o"
CARGO_TARGET_DIR="${work}/elsewhere" check_stale "stale .o in CARGO_TARGET_DIR" "$t" "under ${work}/elsewhere" "${work}/elsewhere/debug/deps/x.rcgu.o"

t="${work}/symlink"; tree "$t"; stale "${work}/linked/debug/deps/x.rcgu.o"
ln -s "${work}/linked" "$t/target"
check_stale "stale .o behind a symlinked target" "$t" "under ${work}/linked" "${work}/linked/debug/deps/x.rcgu.o"

t="${work}/build-dir"; tree "$t"; mkdir -p "$t/.cargo"
printf '[build]\nbuild-dir = "intermediates"\n' >"$t/.cargo/config.toml"
stale "$t/intermediates/debug/deps/x.rcgu.o"
check_stale "stale .o in a separate build-dir" "$t" "under $t/intermediates" "$t/intermediates/debug/deps/x.rcgu.o"

t="${work}/nested-target"; tree "$t"; mkdir -p "$t/fuzz/.cargo"
printf '[build]\ntarget-dir = "cache"\n' >"$t/fuzz/.cargo/config.toml"
stale "$t/fuzz/cache/debug/deps/x.rcgu.o"
check_stale "stale .o in a nested workspace's own target-dir" "$t" "under $t/fuzz/cache" "$t/fuzz/cache/debug/deps/x.rcgu.o"

t="${work}/inner-link"; tree "$t"; stale "${work}/hidden/x.rcgu.o"; mkdir -p "$t/target/debug"
ln -s "${work}/hidden" "$t/target/debug/deps"
expect fail "a directory symlink inside a target dir" "$t" "is a directory symlink inside a target dir"

t="${work}/unreadable"; tree "$t"; mkdir -p "$t/target/debug/locked"
stale "$t/target/debug/locked/deps/x.rcgu.o"; chmod 000 "$t/target/debug/locked"
expect fail "an unreadable dir under target" "$t" "cannot read"
chmod 755 "$t/target/debug/locked"

# ── --clean ────────────────────────────────────────────────────────────────
clean() { "${GUARD}" --clean "$@"; }

t="${work}/clean-ok"; tree "$t"
stale "$t/target/debug/deps/x.rcgu.o"; stale "$t/fuzz/target/release/deps/y.rlib"
stale "$t/.shux/out/test-inventory/target/debug/deps/z.rlib"; touch "$t/keep.rs"
clean "$t" >/dev/null
[[ -d "$t/target" && -z "$(find "$t/target" "$t/fuzz/target" "$t/.shux/out/test-inventory/target" -mindepth 1)" ]] \
  || die "--clean left artifacts in an owned build dir"
[[ -e "$t/keep.rs" ]] || die "--clean deleted a source file"

t="${work}/clean-symlink"; tree "$t"; stale "${work}/linked2/debug/deps/x.rcgu.o"
ln -s "${work}/linked2" "$t/target"
out="$(clean "$t" 2>&1)"
[[ -L "$t/target" && -e "${work}/linked2/debug/deps/x.rcgu.o" && "${out}" == *"not cleaning $t/target"* ]] \
  || die "--clean emptied or broke a symlinked target:"$'\n'"${out}"

# A configured target dir is never emptied: not outside the repo, not inside
# it (here: src/), not when its name smuggles a newline.
t="${work}/clean-configured"; tree "$t"
mkdir -p "$t/src" "${work}/victim"; touch "$t/src/precious.rs" "${work}/victim/sentinel"
for dir in "${work}/victim" "$t/src" "$t/target"$'\n'"${work}/victim"; do
  CARGO_TARGET_DIR="${dir}" clean "$t" >/dev/null 2>&1 || true
done
[[ -e "$t/src/precious.rs" && -e "${work}/victim/sentinel" ]] \
  || die "--clean deleted inside a configured CARGO_TARGET_DIR"

t="${work}/clean-readonly"; tree "$t"
stale "$t/target/debug/deps/z.rlib"; chmod 555 "$t/target/debug/deps"
if clean "$t" >/dev/null 2>&1; then die "--clean reported success on a dir it could not empty"; fi
chmod 755 "$t/target/debug/deps"

t="${work}/clean-locked"; tree "$t"; mkdir -p "$t/target/secret"; chmod 000 "$t/target"
if clean "$t" >/dev/null 2>&1; then die "--clean reported success on a target it could not read"; fi
chmod 755 "$t/target"

echo "✓ target hygiene self-test: every defect caught for the right reason; --clean touches only owned build dirs"
