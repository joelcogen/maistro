#!/usr/bin/env bash
# Build/install without starting the daemon or changing existing configuration.
set -euo pipefail
umask 077
source_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
config_dir="$HOME/.config/maistro"
config_file="$config_dir/config.toml"
cd -- "$source_dir"
cargo build --release --locked
install -Dm755 target/release/maistro "$HOME/.local/bin/maistro"
mkdir -p -- "$config_dir/prompts" "$HOME/.config/systemd/user"
chmod 700 -- "$config_dir"
if [[ ! -e "$config_file" ]]; then
    cp -- config.example.toml "$config_file"
fi
chmod 600 -- "$config_file"
cp -- config.example.toml "$config_dir/config.example.toml"
cp -- examples/project-prompt.md "$config_dir/prompts/example.md"
install -m644 systemd/maistro.service "$HOME/.config/systemd/user/maistro.service"
printf 'Installed. Edit %s using config.example.toml as the reference.\n' "$config_file"
printf '%s\n' \
    'Existing configuration was preserved; legacy pipeline settings require migration.' \
    'Use your normal Codex login, AppFlowy MCP, Git credentials and gh login.' \
    'Validate: maistro --config ~/.config/maistro/config.toml validate' \
    'Reload service definition: systemctl --user daemon-reload' \
    'Start: systemctl --user start maistro' \
    'Stop: systemctl --user stop maistro' \
    'Logs: journalctl --user -u maistro -f'
