#!/usr/bin/env bash
# Launches the built macOS app and checks that it really records what is
# copied: text (with diacritics), a PNG image and a file from Finder.
# Usage: scripts/macos-smoke-test.sh path/to/vrun.clip.app
set -euo pipefail
export LC_ALL=en_US.UTF-8 LANG=en_US.UTF-8

APP="$1"
DATA="$HOME/Library/Application Support/com.vrun.clip"
DB="$DATA/clips.db"
LOGS="$HOME/Library/Logs/com.vrun.clip"
rm -rf "$DATA"

"$APP/Contents/MacOS/vrun-clip" --autostart >/tmp/vrun-stdout.log 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null || true' EXIT

fail() {
  echo "FAIL: $*"
  echo "--- app stdout/stderr"; cat /tmp/vrun-stdout.log || true
  echo "--- app log"; cat "$LOGS"/*.log 2>/dev/null || true
  echo "--- items"; sqlite3 "$DB" "select id, kind, preview, source_app from items" 2>/dev/null || true
  exit 1
}

query() { sqlite3 "$DB" "$1" 2>/dev/null || true; }

# Waits up to 10 s until the SQL query returns a non-empty, non-zero result.
wait_for() {
  for _ in $(seq 1 40); do
    r=$(query "$1")
    if [ -n "$r" ] && [ "$r" != "0" ]; then echo "$r"; return 0; fi
    sleep 0.25
  done
  return 1
}

sleep 6
kill -0 $PID 2>/dev/null || fail "app exited during startup"
[ -f "$DB" ] || fail "database was not created"

# 1. Text
STAMP="vrun smoke $(date +%s) Příliš žluťoučký kůň"
printf '%s' "$STAMP" | pbcopy
wait_for "select count(*) from items where kind='text' and text = '$STAMP'" >/dev/null || fail "text clip not recorded"
echo "ok: text recorded"

# 2. PNG image (64x48)
python3 - <<'PY'
import struct, zlib
w, h = 64, 48
raw = b"".join(b"\x00" + bytes([255, 80, 0, 255]) * w for _ in range(h))
def chunk(t, d):
    return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)) \
    + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")
open("/tmp/vrun-test.png", "wb").write(png)
PY
osascript -e 'set the clipboard to (read (POSIX file "/tmp/vrun-test.png") as «class PNGf»)'
dims=$(wait_for "select image_w || 'x' || image_h from items where kind='image'") || fail "image clip not recorded"
[ "$dims" = "64x48" ] || fail "image has wrong size: $dims"
ls "$DATA"/images/*.png "$DATA"/thumbs/*.png >/dev/null || fail "image files missing"
echo "ok: image recorded ($dims)"

# 3. File (as copied in Finder)
osascript -e 'set the clipboard to (POSIX file "/etc/hosts")'
wait_for "select count(*) from items where kind='files' and text like '%hosts%'" >/dev/null || fail "file clip not recorded"
echo "ok: file recorded"

# Search is covered by the Rust unit tests (`cargo test` on macOS). It can't be
# checked here: the system sqlite3 CLI is older than the app's bundled SQLite and
# does not know the `trigram remove_diacritics` tokenizer.

kill -0 $PID 2>/dev/null || fail "app crashed during the test"
echo "--- items"; query "select id, kind, preview, source_app from items"
echo "PASS"
