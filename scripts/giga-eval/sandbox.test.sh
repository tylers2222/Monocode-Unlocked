#!/usr/bin/env bash
# sandbox_allowed on the lines from review round 2 of #163 (r4328192833),
# plus the puppet as the app starts it. Run: bash scripts/giga-eval/sandbox.test.sh
export GIGA_EVAL_OUT=/tmp/sandbox-test GIGA_EVAL_HOME=/tmp/sandbox-test-home
source "$(dirname "$0")/lib.sh"
source "$(dirname "$0")/sandbox.sh"

APP="/private/tmp/sandbox-test/apps/scenario/MonoCode GigaEval.app/Contents/MacOS/monocode"
NODE="$(realpath "$(command -v node)")"
FX="$EVAL_HOME/.local/bin/fx"
failed=0
check() {
  local want="$1" exe="$2" args="$3" got=deny
  sandbox_allowed "$exe" "$args" "$APP" && got=allow
  if [[ "$got" == "$want" ]]; then echo "ok   $got: $args"; else echo "FAIL $got, expected $want: $args"; failed=1; fi
}

check allow "$APP" "$APP app-mcp"
check deny "$HOME/.local/bin/claude" "claude -p --mcp-config {\"mcpServers\":{\"monocode\":{\"command\":\"$APP\",\"args\":[\"app-mcp\"]}}}"
check deny /opt/homebrew/Cellar/gh/2.74.1/bin/gh "/opt/homebrew/bin/gh api /user"
check deny "$NODE" "node /tmp/evil.js $FX x"
check allow "$NODE" "node $FX acp"
check allow /usr/bin/env "/usr/bin/env node $FX models --json"
check deny /usr/bin/env "/usr/bin/env gh api /user"
check allow /usr/bin/git "git -C /tmp/x status"
check allow "$NODE" "(node)"
check deny /opt/homebrew/Cellar/gh/2.74.1/bin/gh "(gh)"
exit "$failed"
