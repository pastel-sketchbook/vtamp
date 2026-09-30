#!/usr/bin/env bash
# Invoked by tmux's asynchronous #() job. No shell evaluation of user options.
set -u

socket=${1:?Expected tmux socket path}
vtamp_binary=$(tmux -S "$socket" show-options -gqv @vtamp-bin 2>/dev/null)
max_width=$(tmux -S "$socket" show-options -gqv @vtamp-max-width 2>/dev/null)
show_artist=$(tmux -S "$socket" show-options -gqv @vtamp-show-artist 2>/dev/null)

if [[ -z $vtamp_binary ]]; then
    vtamp_binary=$(command -v vtamp 2>/dev/null || true)
    if [[ -z $vtamp_binary && -x $HOME/.cargo/bin/vtamp ]]; then
        vtamp_binary=$HOME/.cargo/bin/vtamp
    fi
fi

if [[ -z $vtamp_binary ]]; then
    printf '\n'
    exit 0
fi
if [[ ! $max_width =~ ^[0-9]{1,3}$ ]] || (( 10#$max_width < 20 || 10#$max_width > 200 )); then
    max_width=50
else
    max_width=$((10#$max_width))
fi

args=(tmux status --max-width "$max_width")
if [[ $show_artist == on ]]; then
    args+=(--show-artist)
fi
"$vtamp_binary" "${args[@]}" 2>/dev/null || printf '\n'
