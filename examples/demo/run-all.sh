#!/usr/bin/env bash
# Run every demo case through nixplain.
#
#   ./run-all.sh                 one line per case (--short)
#   ./run-all.sh full            full diagnostics
#   ./run-all.sh full eval-type  only cases whose name contains "eval-type"
#   LOGS=dir ./run-all.sh        also save each internal-json log to dir/
#
# NIXPLAIN defaults to the binary built in this repository.
set -u
cd "$(dirname "$0")"
NIXPLAIN=${NIXPLAIN:-../../target/debug/nixplain}
mode=${1:-short}
filter=${2:-}
flags=()
[ "$mode" = short ] && flags+=(--short)

hosts=$(nix eval --json .#nixosConfigurations --apply builtins.attrNames | tr -d '[]"' | tr , ' ')
packages=$(nix eval --json .#packages.x86_64-linux --apply builtins.attrNames | tr -d '[]"' | tr , ' ')
homes=$(nix eval --json .#homeConfigurations --apply builtins.attrNames | tr -d '[]"' | tr , ' ')

run() {
  local name=$1; shift
  [[ "$name" == *"$filter"* ]] || return 0
  local log=()
  [ -n "${LOGS:-}" ] && mkdir -p "$LOGS" && log=(--save-log "$LOGS/$name.jsonl")
  if [ "$mode" = short ]; then
    printf '%-34s ' "$name"
  else
    printf '\n\033[1m━━━ %s ━━━\033[0m\n' "$name"
  fi
  "$NIXPLAIN" "${flags[@]}" "${log[@]}" "$@" 2>/dev/null || true
}

for h in $hosts; do run "$h" os --host "$h"; done
for u in $homes; do run "home-$u" home --user "$u"; done
for p in $packages; do run "$p" build ".#$p" --no-link; done
