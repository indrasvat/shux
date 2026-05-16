# Releases

shux uses Conventional-Commits-driven `semantic-release`, mirroring the
pattern used by [`indrasvat/vicaya`](https://github.com/indrasvat/vicaya).
A single workflow (`.github/workflows/release.yml`) runs on every push
to `main` against a `macos-latest` runner. Inside the workflow:

1. semantic-release analyzes commit history, computes the next
   version, bumps `Cargo.toml`, updates `CHANGELOG.md`.
2. `scripts/build-release.sh` is the `prepareCmd` of
   `@semantic-release/exec` — it cross-compiles four binaries, all
   embedding the freshly-bumped `CARGO_PKG_VERSION`:

   | Target                         | How                              |
   |---|---|
   | `aarch64-apple-darwin`         | native cargo build               |
   | `x86_64-apple-darwin`          | cross via Apple SDK              |
   | `x86_64-unknown-linux-gnu`     | `cargo zigbuild` (glibc 2.17)    |
   | `aarch64-unknown-linux-gnu`    | `cargo zigbuild` (glibc 2.17)    |

3. `@semantic-release/git` commits the version bump (`[skip ci]` so this
   workflow does NOT loop) and pushes a `v<X.Y.Z>` tag.
4. `@semantic-release/github` creates the GitHub release and uploads the
   four `.tar.gz` archives plus their `.sha256` sidecars.

## Bootstrap (first-ever release)

semantic-release defaults to `v1.0.0` for the very first release without
a prior tag. To start at `v0.1.0`, use the manual `workflow_dispatch`
trigger in `release.yml`:

```bash
gh workflow run release.yml -f version=0.1.0
```

This skips semantic-release, runs `set-version.sh` + `build-release.sh`,
and creates the `v0.1.0` GitHub release directly. Subsequent `feat:` /
`fix:` commits on `main` then auto-bump from `v0.1.0`.

## Local testing

```bash
make release-build      # build host binary into staging/<triple>/shux
make release-package    # HOST_ONLY=1 → package whatever staging has
```
