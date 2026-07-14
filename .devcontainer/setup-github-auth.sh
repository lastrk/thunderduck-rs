#!/bin/bash
# Authenticate the `gh` CLI inside the devcontainer using a token captured
# from the host's `gh auth token` (extracted into .devcontainer/.github-token
# by devcontainer.json's initializeCommand).
#
# Idempotent: safe to re-run. If the host had no gh CLI auth, exits 0 so
# container creation isn't blocked.
#
# Why --insecure-storage: by default gh stores credentials in the OS
# keyring (libsecret on Linux). Inside a fresh container there is no
# keyring daemon, and gh can hang waiting for D-Bus (see cli/cli#8814).
# Plain-file storage in ~/.config/gh/hosts.yml is appropriate here — the
# container filesystem is already isolated to the local user.
#
# Why `timeout`: belt-and-suspenders so a misbehaving gh can never block
# container creation indefinitely.

set -euo pipefail

TOKEN_FILE=/workspace/.devcontainer/.github-token

if [ ! -s "$TOKEN_FILE" ]; then
    echo "GitHub auth: no token captured from host (file '$TOKEN_FILE' missing or empty)."
    echo "  Hint: run 'gh auth login' on the host, then rebuild the container."
    exit 0
fi

if timeout 30s gh auth login \
        --hostname github.com \
        --insecure-storage \
        --with-token < "$TOKEN_FILE"; then
    echo "GitHub auth: gh CLI authenticated using token from host."
else
    status=$?
    if [ "$status" -eq 124 ]; then
        echo "GitHub auth: 'gh auth login' timed out after 30s; skipping." >&2
    else
        echo "GitHub auth: 'gh auth login' failed (exit $status); skipping." >&2
    fi
    exit 0
fi
