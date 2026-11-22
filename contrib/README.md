# Scheduled health check

gander depends on CLIs that change without notice. A model gets renamed, a login
expires, or one media type stops working while the others stay fine. Nothing
tells you, until an agent gets a failed result.

These files run `gander --check --deep` on a schedule and tell you when it fails.

| File | Use it for |
|---|---|
| `gander-watch.sh` | the script that runs the check and sends the alert |
| `launchd/io.github.gander.watch.plist` | macOS |
| `systemd/gander-watch.service` and `.timer` | Linux with systemd |

## Set it up

1. Copy the script onto your `PATH`:

   ```sh
   install -m 755 contrib/gander-watch.sh ~/.local/bin/gander-watch.sh
   ```

2. Run it once by hand. It prints nothing when every media type works:

   ```sh
   gander-watch.sh; echo "exit $?"
   ```

3. Choose the alert command. The script runs it on failure and gives it the
   report on stdin:

   ```sh
   export GANDER_ALERT_CMD='mail -s "gander is failing" you@example.com'
   ```

4. Schedule it. Follow the comments at the top of the launchd or systemd file.
   With cron, add one line (`crontab -e`):

   ```
   0 */6 * * * PATH=$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin GANDER_ALERT_CMD='mail -s "gander is failing" you@example.com' $HOME/.local/bin/gander-watch.sh
   ```

## What to expect

- **Cost.** Each run makes one real call per media type on your primary, and one
  more on your fallback. That is up to six calls. Every 6 hours is a good default.
- **Time.** A run takes 2 to 4 minutes when the backends are healthy. A failed
  probe waits 45 seconds and tries once more, and a timeout takes much longer.
- **Failures.** The report names a file with the raw output of the backend for
  each failed probe. The files are in `~/.gander/probe-raw`.
- **Alerts.** At most one alert every 12 hours. Change it with
  `GANDER_ALERT_EVERY` (seconds).
- **Log.** `~/.gander/watch.log` has one line per run, and the full report for
  each failed run.
- **A backend with no audio.** `claude` cannot hear. If it is your only backend,
  the audio probe always fails. Set `GANDER_WATCH_ARGS='--deep=image,video'`.
