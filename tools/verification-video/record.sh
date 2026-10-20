#!/usr/bin/env bash
# Records the OAuth verification demo on Hyprland. A development build of Aerie runs with
# its own scratch state and its own keyring entry, so your installed Aerie is untouched.
# You do each step on screen and press Enter here; the steps become the video's captions,
# and the focused window is logged so the editor can follow it.
#
#   tools/verification-video/record.sh            # then: tools/verification-video/assemble.py <dir>
#
# With AERIE_AUTOMATED=1 nothing is read from the terminal: each step waits for <dir>/next
# to appear instead, for a driver script that does the steps itself.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
out=${1:-"$HOME/Videos/aerie-verification/$(date +%Y%m%d-%H%M%S)"}
monitor=${AERIE_MONITOR:-$(hyprctl monitors -j | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["name"])')}

for tool in gpu-screen-recorder hyprctl python3 cargo; do
  command -v "$tool" >/dev/null || { echo "missing: $tool" >&2; exit 1; }
done
client="$HOME/.config/aerie/oauth-client.json"
[ -f "$client" ] || { echo "missing OAuth client file: $client" >&2; exit 1; }

mkdir -p "$out/config/aerie" "$out/state"
cp "$client" "$out/config/aerie/oauth-client.json"
hyprctl monitors -j > "$out/monitors.json"

echo "Building Aerie…"
(cd "$repo" && cargo build -q -p aerie-app -p aerie-service)

steps="$here/steps.tsv"
log="$out/steps.jsonl"
focus="$out/focus.jsonl"
: > "$log"; : > "$focus"

cat <<INTRO

Aerie OAuth verification recording
──────────────────────────────────
Before you start:
  • Use the Google account you want reviewers to see (a demo account keeps your own
    file names out of the video).
  • Open a fresh Chrome window (Ctrl+N) on this workspace so no other tabs show; Aerie's
    sign-in opens there.
  • Keep this terminal small and out of the way; press Enter here after each step.

Press Enter to start recording.
INTRO
automated=${AERIE_AUTOMATED:-}
[ -n "$automated" ] || read -r

# HEVC, because H.264 stops at 4096 pixels and HiDPI monitors are often wider.
gpu-screen-recorder -w "$monitor" -f 30 -k hevc -q very_high -cursor yes -o "$out/raw.mp4" &
recorder=$!
start=$(date +%s.%N)
now() { python3 -c "import time; print(round(time.time() - $start, 2))"; }

# Follow the focused window, for the editor's camera.
(
  while kill -0 "$recorder" 2>/dev/null; do
    printf '{"t":%s,"window":%s}\n' "$(now)" "$(hyprctl activewindow -j)" >> "$focus"
    sleep 0.25
  done
) &
follower=$!

XDG_CONFIG_HOME="$out/config" XDG_STATE_HOME="$out/state" "$repo/target/debug/aerie" \
  >"$out/aerie.log" 2>&1 &
app=$!

n=0
while IFS=$'\t' read -r id show caption instruction; do
  [[ -z "$id" || "$id" == \#* ]] && continue
  n=$((n + 1))
  printf '{"id":"%s","show":"%s","t":%s}\n' "$id" "$show" "$(now)" >> "$log"
  printf '\n%2d. %s\n    then press Enter… ' "$n" "$instruction"
  if [ -n "$automated" ]; then
    until [ -e "$out/next" ]; do sleep 0.2; done
    rm -f "$out/next"
  else
    read -r </dev/tty
  fi
done < "$steps"
printf '{"id":"end","show":"","t":%s}\n' "$(now)" >> "$log"

kill -INT "$recorder"; wait "$recorder" 2>/dev/null || true
kill "$follower" 2>/dev/null || true
kill "$app" 2>/dev/null || true
# The development service exits on its own a minute after the app closes.

echo
echo "Recorded to $out"
echo "Next: $here/assemble.py $out"
