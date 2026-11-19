#!/usr/bin/env bash
# Make sure the pinned ast-grep is installed, and print the directory holding
# it on stdout, for `make check-test-timing` to put first on PATH.
#
# The rules in lint/ast-grep/ are pinned to one ast-grep version because a
# different parser can match different nodes, and a rule that silently stops
# matching guards nothing. Prefers an existing install of the pinned version,
# then a pre-built wheel through uv or pip, then npm. Fails loudly rather than
# letting the guard run on whatever it finds.
set -euo pipefail

version=0.51.3

have() { command -v ast-grep >/dev/null && [[ "$(ast-grep --version | awk '{print $2}')" == "$version" ]]; }
found() { dirname "$(command -v ast-grep)"; }
if have; then
    found
    exit 0
fi

echo "▶ installing ast-grep $version" >&2
# Each installer's bin directory goes first on PATH: it need not be on PATH
# already, and an older ast-grep earlier on PATH must not win.
if command -v uv >/dev/null; then
    uv tool install --force "ast-grep-cli==$version" >&2
    PATH="$(uv tool dir --bin):$PATH"
elif command -v pip3 >/dev/null; then
    pip3 install --user "ast-grep-cli==$version" >&2
    PATH="$(python3 -m site --user-base)/bin:$PATH"
elif command -v npm >/dev/null; then
    npm install --global "@ast-grep/cli@$version" >&2
    PATH="$(npm prefix --global)/bin:$PATH"
else
    echo "ensure-ast-grep: no uv, pip3 or npm to install ast-grep $version with" >&2
    exit 1
fi
hash -r
have || {
    echo "ensure-ast-grep: installed ast-grep $version, but \`ast-grep --version\` says: $(ast-grep --version 2>&1 || echo 'not found')" >&2
    exit 1
}
found
