#!/usr/bin/env bash
# Assert `skills/shux/references/gate.md` has not drifted from the shipped gate
# surface, and that THIRD-PARTY-NOTICES carries the required attribution.
#
# Docs drift silently: a reference can name a TOML key the parser rejects (a real
# bug found on task 084 — `masks` vs `mask`, which fails a run with exit 2) and no
# test notices, because no test reads the doc. This check reads both sides.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DOC="$ROOT/skills/shux/references/gate.md"
SCENARIO_RS="$ROOT/crates/shux/src/gate/scenario.rs"
STATUS_RS="$ROOT/crates/shux-vt/src/gate.rs"
NOTICES="$ROOT/THIRD-PARTY-NOTICES"

fail=0
err() { printf '  \033[31m✗\033[0m %s\n' "$1"; fail=1; }
ok()  { printf '  \033[32m✓\033[0m %s\n' "$1"; }

# grep that tolerates no-match under `set -e`. ONLY for capturing output — never in
# an `if`, because the `|| true` would make the condition unconditionally true.
sgrep() { grep "$@" || true; }

for f in "$DOC" "$SCENARIO_RS" "$STATUS_RS" "$NOTICES"; do
  [[ -f "$f" ]] || { err "missing required file: ${f#"$ROOT"/}"; exit 1; }
done

printf '\033[34m▶ gate doc drift\033[0m\n'

# ── 1. Step actions ──────────────────────────────────────────────────────────
# Truth: the `action()` match arms. Doc: backticked tokens in the Steps table's
# first column, which starts after the `| Action | Fields |` header.
shipped_actions=$(
  sed -n '/fn action(&self)/,/^    }$/p' "$SCENARIO_RS" \
    | sgrep -o '=> "[a-z_]*"' | sed 's/=> "//; s/"//' | sort -u
)
doc_actions=$(
  sed -n '/^| Action | Fields |/,/^$/p' "$DOC" \
    | sgrep '^| `' | sed 's/^| //; s/ *|.*//' \
    | tr -d '`' | tr '/' '\n' | tr -d ' ' | sgrep -v '^$' | sort -u
)
if [[ "$shipped_actions" == "$doc_actions" ]]; then
  ok "step actions match ($(echo "$shipped_actions" | wc -l | tr -d ' ') actions)"
else
  err "step actions drifted between gate.md and Step::action()"
  diff <(echo "$shipped_actions") <(echo "$doc_actions") \
    | sed 's/^</    only in code: /; s/^>/    only in doc:  /' | sgrep -E 'only in'
fi

# ── 2. Exit codes ────────────────────────────────────────────────────────────
# Truth: the distinct codes GateStatus::exit_code() can return.
shipped_codes=$(
  sed -n '/pub fn exit_code(self)/,/^    }$/p' "$STATUS_RS" \
    | sgrep -oE '=> [0-9]+,' | sgrep -oE '[0-9]+' | sort -un
)
doc_codes=$(
  sed -n '/^| Code | Meaning |/,/^$/p' "$DOC" \
    | sgrep -E '^\| [0-9]+ \|' | awk -F'|' '{gsub(/ /,"",$2); print $2}' | sort -un
)
if [[ "$shipped_codes" == "$doc_codes" ]]; then
  ok "exit codes match ($(echo "$shipped_codes" | tr '\n' ' ' | sed 's/ $//'))"
else
  err "exit-code table drifted from GateStatus::exit_code()"
  diff <(echo "$shipped_codes") <(echo "$doc_codes") \
    | sed 's/^</    only in code: /; s/^>/    only in doc:  /' | sgrep -E 'only in'
fi

# Exit 4 is reserved for CLI-level I/O and must never appear as a status code.
if echo "$shipped_codes" | grep -qx 4; then
  err "GateStatus::exit_code() returns the reserved CLI code 4"
else
  ok "exit 4 stays reserved for CLI-level I/O errors"
fi

# ── 3. Scenario TOML keys ────────────────────────────────────────────────────
# Truth: the field names on the Raw* deserialization structs. Every backticked
# token in the doc that looks like a scenario key must be one the parser accepts.
# This is the check that would have caught `masks` (parser: `mask`).
raw_keys=$(
  sed -n '/^struct Raw/,/^}/p' "$SCENARIO_RS" \
    | sgrep -oE '^ *(pub )?[a-z_]+:' | tr -d ' :' | sed 's/^pub//' | sort -u
)
# Keys the doc is allowed to name because the parser genuinely accepts them.
for key in mask steps env terminal cwd command name description deadline_ms; do
  if echo "$raw_keys" | grep -qx "$key"; then
    ok "scenario key \`$key\` is accepted by the parser"
  else
    err "gate.md documents scenario key \`$key\` but no Raw* struct accepts it"
  fi
done
# The specific 084 regression: the plural must never come back as a USABLE key — either
# as a TOML assignment, a `[[…masks]]` table, or a field named in the steps table. Prose
# that warns readers off `masks` is fine and must not trip this.
masks_pat='masks *=|\[\[[^]]*masks|^\|.*masks'
if grep -qE "$masks_pat" "$DOC"; then
  err "gate.md presents \`masks\` as a key; the parser's key is \`mask\` (exit 2 if copied)"
  grep -nE "$masks_pat" "$DOC" | sed 's/^/      /'
else
  ok "gate.md uses \`mask\`, not the rejected \`masks\`"
fi

# ── 3b. Verb-usage traps proved by hand on task 085 ──────────────────────────
# `gate init` takes a NAME (one path component) and appends `.toml`. Documenting
# `gate init scenario.toml` makes a reader create `scenario.toml.toml`.
if grep -qE 'gate init [^ ]*\.toml' "$DOC"; then
  err "gate.md shows \`gate init <file>.toml\`; init takes a NAME and appends .toml"
else
  ok "gate.md uses \`gate init <name>\`, not a .toml path"
fi

# The runtime dir is not in the daemon's argv, so a pattern kill is both unreliable
# and destructive to other checkouts. The pidfile is exact.
if grep -qE '^[^#]*pkill' "$DOC"; then
  err "gate.md recommends pkill; teach the \$XDG_RUNTIME_DIR/shux/shux.pid reap instead"
else
  ok "gate.md does not recommend pattern-killing daemons"
fi

# ── 4. Attribution (task 085 Testing Matrix L1) ───────────────────────────────
printf '\033[34m▶ attribution\033[0m\n'
if grep -q 'Apache-2.0\|Apache License 2.0' "$NOTICES"; then
  ok "THIRD-PARTY-NOTICES names Apache-2.0"
else
  err "THIRD-PARTY-NOTICES does not name Apache-2.0"
fi
while IFS='|' read -r label pattern; do
  if grep -qiE "$pattern" "$NOTICES"; then
    ok "attribution covers: $label"
  else
    err "attribution missing the adapted item: $label"
  fi
done <<'ITEMS'
report/xfail schema shape + exit policy|xfail schema shape and exit policy
`action:` scenario envelope|action.*-tagged scenario envelope
skip-if-default capture discipline|skip-if-default capture discipline
condition-hold settle|condition-hold settle
cast serializer|cast.{0,3} serializer
ITEMS

echo
if [[ $fail -eq 0 ]]; then
  printf '\033[32m✓ gate docs match the shipped surface\033[0m\n'
else
  printf '\033[31m✗ gate doc drift detected\033[0m\n'
fi
exit $fail
