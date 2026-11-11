#!/usr/bin/env bash
# Install from this checkout, then interactively configure and validate Maistro.
set +x
set -euo pipefail
umask 077

source_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
config_dir="$HOME/.config/maistro"
config_file="$config_dir/config.toml"
environment_file="$config_dir/environment"
binary="$HOME/.local/bin/maistro"

cd -- "$source_dir"
cargo build --release --locked
install -Dm755 target/release/maistro "$binary"
mkdir -p -- "$config_dir"
chmod 700 -- "$config_dir"
if [[ ! -e "$config_file" ]]; then
    if [[ -f "$source_dir/config.toml" ]]; then
        cp -- "$source_dir/config.toml" "$config_file"
    else
        cp -- "$source_dir/config.example.toml" "$config_file"
    fi
fi
chmod 600 -- "$config_file"

if [[ -f "$environment_file" ]]; then
    printf 'Using existing environment file: %s\n' "$environment_file"
else
    cat <<'INSTRUCTIONS'
Create a GitHub personal access token (classic):
  https://github.com/settings/tokens/new
1. Sign in with an account that has Write access to your repositories.
   Repository owners can use github.auto_merge = false (the default).
   Automatic merging requires a non-administrator account with no
   protection-bypass role.
2. Set Note to Maistro and choose an expiration (for example, 30 days).
3. Select only the top-level repo scope, leaving its child scopes selected.
   Leave workflow, admin:org, delete_repo and all other top-level scopes unchecked.
4. Generate the token. If your organization uses SAML SSO, authorize it via
   Configure SSO at https://github.com/settings/tokens.
Use a classic token: this version's Checks API access requires it. Classic tokens
cover all repositories available to the account, so limit that account's access.
Organizations that block classic tokens cannot use this setup.
INSTRUCTIONS

    while true; do
        printf 'GitHub token (input hidden): ' >&2
        if ! IFS= read -r -s token; then
            printf '\nNo token supplied; installation stopped before validation.\n' >&2
            exit 1
        fi
        printf '\n' >&2
        # GitHub tokens contain only these characters; keep the environment file
        # compatible with both shell sourcing and systemd without interpreting input.
        if [[ "$token" =~ ^[a-zA-Z0-9_]+$ ]]; then
            break
        fi
        printf 'Enter a nonempty GitHub token using letters, digits and underscores.\n' >&2
    done

    temporary_environment=$(mktemp "$config_dir/.environment.XXXXXX")
    trap 'rm -f -- "$temporary_environment"' EXIT
    printf 'MAISTRO_GITHUB_TOKEN=%s\n' "$token" > "$temporary_environment"
    chmod 600 -- "$temporary_environment"
    mv -- "$temporary_environment" "$environment_file"
    unset token
fi
chmod 600 -- "$environment_file"
# Load saved settings for validation, just as in the manual startup instructions.
set -a
. "$environment_file"
set +a

printf '\nEdit %s: configure your projects and keep github.token_env = "MAISTRO_GITHUB_TOKEN".\n' "$config_file"
# EDITOR is a user-selected shell command (e.g. "code --wait"). Pass the
# configuration path separately so spaces and shell characters in HOME are safe.
bash -c "${EDITOR:-vi} \"\$1\"" maistro-editor "$config_file"
chmod 600 -- "$config_file"
printf '\nValidating configuration...\n'
"$binary" -c "$config_file" validate
printf '\nInstallation and validation succeeded. See README.md, "Validate and start", to start the user service.\n'
