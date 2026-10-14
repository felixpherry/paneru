#!/usr/bin/env bash
# Build, sign and restart Paneru so macOS keeps its Accessibility and Screen
# Recording grants across rebuilds. One-time setup: README › "Keeping
# permissions across rebuilds". Extra arguments go to `cargo install`.
set -euo pipefail

IDENTITY="${PANERU_CODESIGN_IDENTITY:-paneru-dev}"
LABEL=com.github.karinushka.paneru
TARGET="gui/$(id -u)/$LABEL"
BIN="${CARGO_HOME:-$HOME/.cargo}/bin/paneru"

cd "$(dirname "$0")/.."
cargo install --path . --locked "$@"

# Pattern matches, not `| grep -q`: under pipefail an early grep exit can
# fail the pipeline even on a match.
if [[ $(security find-identity -p codesigning) == *"\"$IDENTITY\""* ]]; then
  codesign --force --sign "$IDENTITY" --identifier "$LABEL" "$BIN"
else
  echo "warning: no '$IDENTITY' code-signing certificate, so $BIN stays ad-hoc signed" >&2
  echo "         and macOS drops its Accessibility and Screen Recording grants on every" >&2
  echo "         rebuild. See README › Keeping permissions across rebuilds." >&2
fi

pid() { launchctl print "$TARGET" 2>/dev/null | awk '$1 == "pid" { print $3 }' || true; }

if launchctl print "$TARGET" >/dev/null 2>&1; then
  old=$(pid)
  launchctl kickstart -k "$TARGET" || true
  new=
  for _ in {1..120}; do # ~12 s: launchd throttles a respawn for up to 10 s
    new=$(pid)
    [[ -n $new && $new != "$old" ]] && break
    sleep 0.1
  done
  # Known quirk: `kickstart -k` sometimes kills without restarting.
  [[ -n $new && $new != "$old" ]] || launchctl kickstart "$TARGET"
else
  echo "note: the $LABEL service is not loaded; run 'paneru install' once." >&2
fi

codesign -dvv "$BIN" 2>&1 | grep -E 'Identifier|Authority|Signature'
