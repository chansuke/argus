#!/usr/bin/env bash
# Tests check-escape-hatches.sh against a throwaway repository.
set -euo pipefail

check=$(cd "$(dirname "$0")" && pwd)/check-escape-hatches.sh
repo=$(mktemp -d)
trap 'rm -rf "$repo"' EXIT
cd "$repo"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
git init -q -b main
git -c user.name=t -c user.email=t@example.com commit -q --allow-empty -m base
failures=0
expect() { # WANT(pass|fail) DESCRIPTION FILE CONTENT
  local want=$1 what=$2 status=0
  mkdir -p "$(dirname "$3")"
  printf '%s\n' "$4" >"$3"
  git add "$3"
  "$check" --staged >/dev/null 2>&1 || status=$?
  if { [ "$want" = pass ] && [ "$status" -ne 0 ]; } || { [ "$want" = fail ] && [ "$status" -ne 1 ]; }; then
    echo "FAIL: $what (exit $status)"
    failures=1
  fi
  git rm -q --cached "$3"
  rm -f "$3"
}

expect pass "plain code" src/a.rs 'fn main() {}'
expect fail "a sleep in an integration test" crates/x/tests/a.rs '    std::thread::sleep(Duration::from_millis(50));'
expect fail "a sleep in a unit test module" src/a.rs $'fn f() {}\n#[cfg(test)]\nmod tests {\n    fn t() { std::thread::sleep(D); }\n}'
expect pass "a production poll interval (clippy and review judge it)" src/a.rs $'fn poll() {\n    std::thread::sleep(every);\n}\n#[cfg(test)]\nmod tests {}'
expect pass "a word that ends in sleep" src/a.rs 'fn may_sleep_now() {}'
expect fail "an ignored test without a reason" src/a.rs '#[ignore]'
expect pass "an ignored test with a reason" src/a.rs '#[ignore = "reads real transcripts named by ARGUS_TRANSCRIPTS"]'
expect fail "a new todo! stub" src/a.rs '    todo!("not implemented")'
expect fail "a new unimplemented! stub" src/a.rs '    unimplemented!()'
expect fail "a TODO comment" src/a.rs 'fn main() {} // TODO: handle errors'
expect fail "a workaround comment" src/a.rs '// Workaround for the flaky test'
expect pass "a word in a string, not a comment" src/a.rs 'let s = "TODO list";'
expect pass "an attribute is not a comment" src/a.rs '#[cfg(test)]'
expect fail "ts-ignore" ui/a.ts '// @ts-ignore'
expect fail "ts-expect-error without a reason" ui/a.ts '// @ts-expect-error'
expect pass "ts-expect-error with a reason" ui/a.tsx '// @ts-expect-error -- the RN types miss this prop'
expect fail "eslint-disable without a reason" ui/a.ts '// eslint-disable-next-line @typescript-eslint/no-explicit-any'
expect pass "eslint-disable with a reason" ui/a.ts '// eslint-disable-next-line @typescript-eslint/no-explicit-any -- JSON from the wire'
expect fail "a double cast" ui/a.ts 'const t = x as unknown as Transport;'
expect pass "prose in docs" docs/usage.md 'For now, sleep(1). TODO list'
expect pass "generated TS" ui/packages/argus-client/src/gen/api.ts 'const x = y as unknown as Z;'

# A range reports the line number in the new file.
printf 'a\nb\n' >f.rs
git add f.rs
git -c user.name=t -c user.email=t@example.com commit -q -m two
printf 'a\nb\n// TODO c\n' >f.rs
git add f.rs
git -c user.name=t -c user.email=t@example.com commit -q -m three
reported=$("$check" HEAD~1 HEAD 2>&1 || true)
if ! grep -q '^f.rs:3: // TODO c$' <<<"$reported"; then
  echo "FAIL: the line number of an added line"
  failures=1
fi

if [ "$failures" -ne 0 ]; then
  exit 1
fi
echo "check-escape-hatches: all cases pass"
