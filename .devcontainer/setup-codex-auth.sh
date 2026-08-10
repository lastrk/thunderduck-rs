#!/usr/bin/env bash
# Authenticate Codex CLI from a host credential captured by initializeCommand.
#
# API keys are the normal choice for repeatable local container creation.
# ChatGPT Enterprise access tokens take precedence when both are available.
# A copied ChatGPT auth cache covers interactive subscription sign-in.
# With none of these, keep container creation successful and use device auth manually.

set -euo pipefail

DEVCONTAINER_DIR=/workspace/.devcontainer
ACCESS_TOKEN_FILE="$DEVCONTAINER_DIR/.codex-access-token"
API_KEY_FILE="$DEVCONTAINER_DIR/.codex-api-key"
AUTH_CACHE_FILE="$DEVCONTAINER_DIR/.codex-auth.json"

if [ -s "$ACCESS_TOKEN_FILE" ]; then
    codex login --with-access-token < "$ACCESS_TOKEN_FILE"
    echo "Codex: authenticated with a ChatGPT Enterprise access token."
elif [ -s "$API_KEY_FILE" ]; then
    codex login --with-api-key < "$API_KEY_FILE"
    echo "Codex: authenticated with an OpenAI API key."
elif [ -s "$AUTH_CACHE_FILE" ]; then
    install -m 600 "$AUTH_CACHE_FILE" "$HOME/.codex/auth.json"
    echo "Codex: restored the host ChatGPT login cache."
else
    echo "Codex: no API key or access token captured from the host."
    echo "  Use 'codex login --device-auth' in the container to sign in with ChatGPT."
    exit 0
fi

codex login status
