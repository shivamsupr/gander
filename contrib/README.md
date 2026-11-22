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

- **Cost.** Each run makes three real backend calls, one per media type. Every
  6 hours is a good default.
- **Time.** A run takes 1 to 3 minutes when the backends are healthy, and longer
  when one of them times out.
- **Alerts.** At most one alert every 12 hours. Change it with
  `GANDER_ALERT_EVERY` (seconds).
- **Log.** `~/.gander/watch.log` has one line per run, and the full report for
  each failed run.
- **A backend with no audio.** `claude` cannot hear. If it is your only backend,
  the audio and video probes always fail. Set `GANDER_WATCH_ARGS='--deep=image'`.
