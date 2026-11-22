#!/bin/sh
# gander-watch.sh — run the gander deep check and tell someone when it fails.
#
# Schedule it with launchd, systemd or cron (see the README next to this file).
# It prints nothing and exits 0 while every media type works.
#
# Settings (environment variables):
#   GANDER_ALERT_CMD   command to run on failure. It gets the report on stdin.
#                      Example: 'mail -s "gander is failing" you@example.com'
#                      Not set: the report goes to stderr.
#   GANDER_ALERT_EVERY seconds between two alerts (default 43200 = 12 hours).
#   GANDER_WATCH_DIR   where the log and the alert stamp go (default ~/.gander).
#   GANDER_WATCH_ARGS  what to run after `gander --check` (default '--deep').
#                      Example: '--deep=image,audio --backend codex'.

DIR="${GANDER_WATCH_DIR:-$HOME/.gander}"
LOG="$DIR/watch.log"
STAMP="$DIR/watch-last-alert"
EVERY="${GANDER_ALERT_EVERY:-43200}"
mkdir -p "$DIR"

now=$(date +%s)
# The word splitting of GANDER_WATCH_ARGS is wanted here.
# shellcheck disable=SC2086
report=$(gander --check ${GANDER_WATCH_ARGS:---deep} 2>&1)
rc=$?

if [ "$rc" -eq 0 ]; then
  echo "$(date '+%Y-%m-%d %H:%M:%S') OK" >> "$LOG"
  exit 0
fi

echo "$(date '+%Y-%m-%d %H:%M:%S') FAILED exit=$rc" >> "$LOG"
echo "$report" | sed 's/^/    /' >> "$LOG"

# One alert per cooldown period. A known outage must not page you every run.
if [ -f "$STAMP" ] && [ $(( now - $(cat "$STAMP") )) -lt "$EVERY" ]; then
  exit "$rc"
fi

message="gander deep check failed (exit $rc) on $(hostname):

$report"

if [ -n "${GANDER_ALERT_CMD:-}" ]; then
  # Write the stamp only after the alert went out. A failed send must not
  # silence the next one.
  if printf '%s\n' "$message" | sh -c "$GANDER_ALERT_CMD"; then
    echo "$now" > "$STAMP"
  else
    echo "$(date '+%Y-%m-%d %H:%M:%S') ALERT COMMAND FAILED" >> "$LOG"
  fi
else
  printf '%s\n' "$message" >&2
  echo "$now" > "$STAMP"
fi
exit "$rc"
