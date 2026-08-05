#!/usr/bin/env bash
set -euo pipefail

root=$(git rev-parse --show-toplevel)
cd "$root"

expected_origin="git@github.com:kylegl/herdr.git"
expected_upstream="https://github.com/herdrdev/herdr.git"
expected_upstream_push="DISABLED"

fail() {
  printf 'fork preflight failed: %s\n' "$1" >&2
  exit 1
}

[[ $(git remote get-url origin) == "$expected_origin" ]] ||
  fail "origin must be $expected_origin"
[[ $(git remote get-url upstream) == "$expected_upstream" ]] ||
  fail "upstream must be $expected_upstream"
[[ $(git remote get-url --push upstream) == "$expected_upstream_push" ]] ||
  fail "upstream push URL must remain $expected_upstream_push"
[[ $(git config --get remote.pushDefault) == "origin" ]] ||
  fail "remote.pushDefault must be origin"

if [[ $(git branch --show-current) == "master" ]]; then
  [[ $(git config --get branch.master.remote) == "origin" ]] ||
    fail "master must track origin/master"
  [[ $(git config --get branch.master.merge) == "refs/heads/master" ]] ||
    fail "master must track origin/master"
fi

printf 'fork preflight passed\n'
printf '  writable: %s\n' "$expected_origin"
printf '  read-only upstream: %s\n' "$expected_upstream"
printf '  branch: %s\n' "$(git branch --show-current)"
