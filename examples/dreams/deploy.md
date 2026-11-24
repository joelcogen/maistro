Deploy Maistro from the updated base branch once the other dreams have finished.
Build the release with `cargo build --locked --release`.

The standard user service runs `~/.local/bin/maistro`. Check that the binary is
installed and `maistro.service` is active. If the build fails or either check
fails, report the blocker. If the new binary matches the installed one, finish
without deploying.

Keep a copy of the installed binary until the restart is scheduled. Stage
`target/release/maistro` in `~/.local/bin` with mode `0755` and rename it over the
installed binary. Preserve the existing service unit, configuration, and
credentials. Schedule the restart through a separate transient user timer so
the dream worker has time to finish before its service stops.

```sh
systemd-run --user --on-active=1min systemctl --user try-restart maistro.service
```

If scheduling fails, restore the previous binary and report the blocker.
Report a successful schedule as a pending restart.
