#!/usr/bin/env bash
# Rejects added lines that put off a fix or switch a check off: sleeps, tests
# ignored without a reason, new todo!() stubs, TODO-style markers, and
# TypeScript or ESLint suppressions without a reason. Only the lines a change
# adds are read, so existing code never fails it.
#
# usage: scripts/check-escape-hatches.sh --staged     (the index against HEAD)
#        scripts/check-escape-hatches.sh BASE [HEAD]  (BASE to HEAD, or to the working tree)
set -euo pipefail

case "${1:-}" in
--staged) diff=(git diff --cached --no-color --no-ext-diff -U0 --) ;;
"")
  echo "usage: $0 --staged | BASE [HEAD]" >&2
  exit 2
  ;;
*) diff=(git diff --no-color --no-ext-diff -U0 "$@" --) ;;
esac

# Each rule: what to read (code or comment), a file glob (`*` also matches `/`),
# an extended regular expression that flags the line, one that clears it again
# (`-` for none), and what to do instead.
rules=(
  $'code\t*.rs\t(^|[^A-Za-z_])sleep\\(\t-\tDo not wait for time to pass. Wait on a channel, a condvar, or the event itself (REVIEW.md: no sleeping tests). In production, block on the thing that changes.'
  $'code\t*.rs\t#\\[ignore\\]\t-\tGive the reason: #[ignore = "why it does not run by default"]. A failing test is fixed or removed, not ignored (REVIEW.md).'
  $'code\t*.rs\t(^|[^A-Za-z_])(todo|unimplemented)!\t-\tImplement it in this change, or leave the feature out. A stub compiles but fails at run time.'
  $'comment\t*\t(^|[^A-Za-z])(TODO|FIXME|HACK|XXX)([^A-Za-z]|$)\t-\tFix it in this change. If it is out of scope, open an issue and describe what the code does now as a fact; better, add a test or a lint that fails until it is fixed.'
  $'comment\t*\t[Ww]ork-?around|[Ff]or now|[Qq]uick fix|[Bb]and-?aid|[Tt]emporary (fix|hack|patch)\t-\tDo not explain away a workaround. Find the cause and fix it; if the cause is upstream, name the upstream behavior and link its issue.'
  $'code\t*.ts*\t@ts-ignore|@ts-nocheck\t-\tFix the type error. If the types are wrong upstream, use `// @ts-expect-error -- <reason>`, which fails once they are fixed.'
  $'code\t*.ts*\t@ts-expect-error\t@ts-expect-error[[:space:]]+--[[:space:]]+[^[:space:]]\tSay why after the directive: `// @ts-expect-error -- <reason>`.'
  $'code\t*.[jt]s*\teslint-disable\teslint-disable[^*]*[[:space:]]--[[:space:]]+[^[:space:]]\tName the rule and say why: `// eslint-disable-next-line <rule> -- <reason>`, or change the code so the rule passes.'
  $'code\t*.ts*\tas unknown as\t-\tA double cast skips the type checker. Narrow the value with a type guard, or fix the declared type.'
)

# The comment part of LINE in FILE, empty when it has none.
comment_of() { # FILE LINE
  case $1 in
  *.rs | *.ts | *.tsx | *.js) [[ $2 == *//* ]] && printf '%s' "//${2#*//}" ;;
  *.sh | *.toml | *.yaml | *.yml | *.nix | tools/*)
    [[ $2 =~ (^|[[:space:]])#([^[!]|$) ]] && printf '%s' "#${2#*#}"
    ;;
  *.tla | *.cfg) [[ $2 == *'\*'* ]] && printf '%s' "${2#*\\\*}" ;;
  *.lean) [[ $2 == *--* ]] && printf '%s' "--${2#*--}" ;;
  esac
  return 0
}

# Prose and generated or locked files are not read.
checked() { # FILE
  case $1 in
  docs/* | *.md | *.lock | *-lock.yaml | */gen/*) return 1 ;;
  scripts/check-escape-hatches.sh | scripts/test-check-escape-hatches.sh) return 1 ;;
  esac
  return 0
}

found=0
file=""
line=0
while IFS= read -r row; do
  case $row in
  '+++ b/'*)
    file=${row#+++ b/}
    continue
    ;;
  '+++ '* | '--- '*) continue ;;
  '@@ '*)
    # @@ -a,b +c,d @@: the next added line is line c of the new file.
    line=${row#*+}
    line=${line%%[, ]*}
    continue
    ;;
  '+'*) ;;
  *) continue ;;
  esac
  text=${row#+}
  if checked "$file"; then
    comment=$(comment_of "$file" "$text")
    for rule in "${rules[@]}"; do
      IFS=$'\t' read -r kind glob pattern unless advice <<<"$rule"
      # shellcheck disable=SC2053 # the pattern is meant to glob
      [[ $file == $glob ]] || continue
      subject=$text
      [ "$kind" = comment ] && subject=$comment
      if [ -n "$subject" ] && grep -Eq -- "$pattern" <<<"$subject" &&
        { [ "$unless" = - ] || ! grep -Eq -- "$unless" <<<"$subject"; }; then
        printf '%s:%s: %s\n    %s\n' "$file" "$line" "${text#"${text%%[![:space:]]*}"}" "$advice" >&2
        found=$((found + 1))
      fi
    done
  fi
  line=$((line + 1))
done < <("${diff[@]}")

if ((found > 0)); then
  echo "$found added line(s) put off a fix or switch off a check. Change the code; do not reword it to pass." >&2
  exit 1
fi
