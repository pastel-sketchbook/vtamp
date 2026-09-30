#!/usr/bin/env bash
# TPM entry point; also supports: run-shell '/path/to/vtamp/vtamp.tmux'
set -eu

plugin_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
socket=$(tmux display-message -p '#{socket_path}')

shell_quote() {
    local value=${1//\'/\'\\\'\'}
    printf "'%s'" "$value"
}

command="exec $(shell_quote "$plugin_dir/scripts/tmux-status.sh") $(shell_quote "$socket")"
# The command itself passes through tmux's format expansion before /bin/sh.
command=${command//#/##}
segment="#($command)"
token='#{vtamp}'

replace_segments() {
    local option value updated
    for option in status-left status-right; do
        value=$(tmux show-options "$@" -qv "$option")
        if [[ $value == *"$token"* ]]; then
            # Literal concatenation avoids Bash-version-specific replacement
            # quoting and special treatment of ampersands in installation paths.
            updated=
            while [[ $value == *"$token"* ]]; do
                updated+="${value%%"$token"*}$segment"
                value=${value#*"$token"}
            done
            tmux set-option "$@" "$option" "$updated$value"
        fi
    done
}

replace_segments -g
# Explicit session overrides should work too, without creating new overrides.
while IFS= read -r session; do
    replace_segments -t "$session"
done < <(tmux list-sessions -F '#{session_id}')
