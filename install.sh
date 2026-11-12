#!/usr/bin/env bash
# Build/install without starting the daemon; preserve configuration and saved token.
set +x # Never trace credential input, even when invoked with bash -x.
set -euo pipefail
umask 077
source_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
config_dir="$HOME/.config/maistro"
config_file="$config_dir/config.toml"
token_file="$config_dir/github-token"
cd -- "$source_dir"
cargo build --release --locked
install -Dm755 target/release/maistro "$HOME/.local/bin/maistro"
mkdir -p -- "$config_dir" "$HOME/.config/systemd/user"
chmod 700 -- "$config_dir"
printf '%s\n' \
    'Create a fine-grained GitHub personal access token for Maistro:' \
    'https://github.com/settings/personal-access-tokens/new' \
    'Select the resource owner and only the repositories Maistro manages.' \
    'Repository permissions: Contents: Read and write (push commits);' \
    'Workflows: Read and write (edit .github/workflows/);' \
    'Pull requests: Read and write (PR mode, including recovery of existing PRs).' \
    'Choose an expiration and renew before it expires. Obtain organization approval if required.' \
    'The token owner must have write access. Branch rules must allow the configured mode;' \
    'a token does not bypass protected branches. Use PR mode when direct pushes are prohibited.'
if [[ -L "$token_file" || ( -e "$token_file" && ! -f "$token_file" ) ]]; then
    printf 'GitHub token path must be a regular file: %s\n' "$token_file" >&2
    exit 1
fi
github_token=${MAISTRO_GITHUB_TOKEN:-}
unset MAISTRO_GITHUB_TOKEN
if [[ -z "$github_token" && ! -s "$token_file" ]]; then
    if [[ ! -t 0 ]]; then
        printf '%s\n' 'Token required. Rerun interactively or supply MAISTRO_GITHUB_TOKEN in the environment.' >&2
        exit 1
    fi
    IFS= read -r -s -p 'Paste the GitHub token (hidden): ' github_token
    printf '\n'
    if [[ -z "$github_token" ]]; then
        printf '%s\n' 'GitHub token cannot be empty.' >&2
        exit 1
    fi
fi
if [[ -n "$github_token" ]]; then
    if [[ ! "$github_token" =~ ^[a-zA-Z0-9_]+$ || ${#github_token} -gt 4096 ]]; then
        printf '%s\n' 'Invalid GitHub token: expected letters, digits and underscores on one line.' >&2
        exit 1
    fi
    token_tmp=$(mktemp "$config_dir/.github-token.XXXXXX")
    trap 'rm -f -- "$token_tmp"' EXIT
    printf '%s\n' "$github_token" > "$token_tmp"
    chmod 600 -- "$token_tmp"
    mv -f -- "$token_tmp" "$token_file"
    trap - EXIT
    unset github_token
    printf 'Saved GitHub token to %s (owner-only access).\n' "$token_file"
else
    printf 'Preserved GitHub token at %s.\n' "$token_file"
fi
chmod 600 -- "$token_file"
if [[ ! -e "$config_file" ]]; then
    cp -- config.example.toml "$config_file"
fi
chmod 600 -- "$config_file"
cp -- config.example.toml "$config_dir/config.example.toml"
install -m644 systemd/maistro.service "$HOME/.config/systemd/user/maistro.service"
printf 'Installed. Edit %s using config.example.toml as the reference.\n' "$config_file"
printf '%s\n' \
    'Existing configuration was preserved; legacy pipeline settings require migration.' \
    'Use your normal Codex login and AppFlowy MCP. Install gh for PR creation.' \
    'Workers use the saved GitHub token for Git and gh; no gh login is required.' \
    'Validate: maistro --config ~/.config/maistro/config.toml validate' \
    'Reload service definition: systemctl --user daemon-reload' \
    'Start: systemctl --user start maistro' \
    'Stop: systemctl --user stop maistro' \
    'Logs: journalctl --user -u maistro -f'
