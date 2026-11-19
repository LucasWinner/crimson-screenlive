#!/usr/bin/env bash
# End-to-end test: streamer -> signaling server -> headless viewer.
# Defaults to the local server (`npm run dev` in signaling/); set
# SCREENLIVE_SERVER=https://... to test against the deployed one.
# Usage: scripts/e2e.sh [test|screen] [extra stream args...]
set -u
cd "$(dirname "$0")/.."
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) export PATH="/c/Program Files/gstreamer/1.0/msvc_x86_64/bin:$PATH"; EXE=.exe ;;
  *) EXE= ;;
esac
SOURCE="${1:-test}"; shift || true
CLI="./target/debug/screenlive-cli$EXE"
ROOM="E2E$(printf '%03d' $((RANDOM % 1000)) | tr '01' 'AB')"
LOG="${TMPDIR:-/tmp}/screenlive-e2e-streamer.log"

export SCREENLIVE_SERVER="${SCREENLIVE_SERVER:-http://127.0.0.1:8787}"
cargo build -q -p screenlive-core || exit 1
curl -sf "$SCREENLIVE_SERVER/health" >/dev/null || { echo "signaling server not reachable at $SCREENLIVE_SERVER (local: cd signaling && npm run dev)"; exit 1; }

AUDIO=test; [ "$SOURCE" = screen ] && AUDIO=no-discord
timeout 40 "$CLI" stream --room "$ROOM" --name Alice --source "$SOURCE" --audio "$AUDIO" "$@" >"$LOG" 2>&1 &
SP=$!
sleep 3
timeout 30 "$CLI" watch --room "$ROOM" --name Bob --headless --seconds 8
RC=$?
kill "$SP" 2>/dev/null; wait "$SP" 2>/dev/null
echo "--- streamer log ($LOG)"; grep -vE "^\s*$" "$LOG" | tail -12
[ $RC -eq 0 ] && echo "E2E PASS" || echo "E2E FAIL ($RC)"
exit $RC
