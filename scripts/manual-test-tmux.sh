#!/usr/bin/env bash
#
# Manual smoke test: builds imscope, then launches one producer
# (imscope-producer-rs's dual_scope example) and two imscope-view
# consumers -- each watching a different scope -- side by side in a tmux
# session, so you can eyeball that live data actually flows end-to-end.
#
# Usage:
#   scripts/manual-test-tmux.sh
#
# Requires: tmux, cargo. Tear down with:
#   tmux kill-session -t imscope-manual-test

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

SESSION="imscope-manual-test"
PRODUCER_NAME="rust_dual_scope_example"

if ! command -v tmux >/dev/null 2>&1; then
  echo "error: tmux is required but not installed" >&2
  exit 1
fi

echo "Building imscope-producer-rs's dual_scope example and imscope-view..."
cargo build -p imscope-producer-rs --example dual_scope
cargo build --bin imscope-view

PRODUCER_BIN="$REPO_ROOT/target/debug/examples/dual_scope"
VIEW_BIN="$REPO_ROOT/target/debug/imscope-view"

# Idempotent: drop any stale session from a previous run.
tmux kill-session -t "$SESSION" 2>/dev/null || true

tmux new-session -d -s "$SESSION" -n imscope \
  "'$PRODUCER_BIN'; exec \$SHELL"
tmux select-pane -t "$SESSION:imscope.0" -T "producer: $PRODUCER_NAME" 2>/dev/null || true

# Give the producer a moment to come up before the consumers try to connect
# (not strictly required -- imscope-view just shows "Connecting..." and
# keeps retrying -- but it makes the first frame appear sooner).
tmux split-window -t "$SESSION:imscope" -h \
  "sleep 2; '$VIEW_BIN' --producer '$PRODUCER_NAME' --scope SineWave; exec \$SHELL"
tmux select-pane -t "$SESSION:imscope.1" -T "consumer: SineWave" 2>/dev/null || true

tmux split-window -t "$SESSION:imscope.1" -v \
  "sleep 2; '$VIEW_BIN' --producer '$PRODUCER_NAME' --scope NoisyIQ --plot scatter; exec \$SHELL"
tmux select-pane -t "$SESSION:imscope.2" -T "consumer: NoisyIQ" 2>/dev/null || true

tmux set-option -t "$SESSION" -g pane-border-status top 2>/dev/null || true
tmux set-option -t "$SESSION" -g pane-border-format " #{pane_index}: #{pane_title} " 2>/dev/null || true
tmux select-layout -t "$SESSION:imscope" tiled

if [ -t 1 ]; then
  tmux attach -t "$SESSION"
else
  echo "Session '$SESSION' started detached. Attach with: tmux attach -t $SESSION"
fi
