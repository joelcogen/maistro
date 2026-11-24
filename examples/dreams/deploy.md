Deploy Maistro after the other dreams have finished. Run the commands from the
updated configured base branch. In PR mode, use a temporary detached worktree at
the fetched base commit for the installation.

The installer uses the existing saved GitHub token and configuration. Reload the
user service manager after installation so it picks up the installed unit. Schedule
the restart in a separate transient user timer so the dream worker can report its
result before Maistro stops.

```sh
systemctl --user is-active --quiet maistro.service &&
    env -u MAISTRO_GITHUB_TOKEN ./install.sh </dev/null &&
    systemctl --user daemon-reload &&
    systemd-run --user --on-active=1min systemctl --user try-restart maistro.service
```

If any command fails, report the failed step and whether installation completed.
Report successful scheduling as a pending restart.
