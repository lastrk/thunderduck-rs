#!/bin/bash
# Generate Claude Code configuration for container
# Uses ANTHROPIC_AUTH_TOKEN environment variable only

DEVCONTAINER_DIR=/workspace/.devcontainer
AUTH_TOKEN_FILE="$DEVCONTAINER_DIR/.claude-auth-token"
BASE_URL_FILE="$DEVCONTAINER_DIR/.claude-base-url"
CUSTOM_HEADERS_FILE="$DEVCONTAINER_DIR/.claude-custom-headers"
OUTPUT=~/.claude.json
SETTINGS_OUTPUT=~/.claude/settings.json

# Ensure .claude directory exists
mkdir -p ~/.claude

# Read authentication from host-extracted files
AUTH_TOKEN=$(cat "$AUTH_TOKEN_FILE" 2>/dev/null || echo "")
BASE_URL=$(cat "$BASE_URL_FILE" 2>/dev/null || echo "")
CUSTOM_HEADERS=$(cat "$CUSTOM_HEADERS_FILE" 2>/dev/null || echo "")

if [ -z "$AUTH_TOKEN" ]; then
    echo "Error: ANTHROPIC_AUTH_TOKEN not set"
    echo "Please set ANTHROPIC_AUTH_TOKEN environment variable before starting container"
    exit 1
fi

echo "Using ANTHROPIC_AUTH_TOKEN for authentication"

# Get last 20 chars of token (Claude Code uses this as the key suffix)
TOKEN_SUFFIX="${AUTH_TOKEN: -20}"

# Create minimal ~/.claude.json
# No MCP servers are pre-registered. Install on demand inside the container and add
# a `mcpServers.<name>` entry here (see CLAUDE.md → "Installing MCP Servers on Demand").
jq -n \
  --arg tokenSuffix "$TOKEN_SUFFIX" \
  '{
    numStartups: 1,
    installMethod: "devcontainer",
    hasCompletedOnboarding: true,
    customApiKeyResponses: {
      approved: [$tokenSuffix],
      rejected: []
    },
    projects: {
      "/workspace": {
        allowedTools: [],
        history: [],
        hasTrustDialogAccepted: true
      }
    }
  }' > "$OUTPUT"

chmod 600 "$OUTPUT"
echo "Generated $OUTPUT"

# Build settings.json with environment variables
ENV_VARS=$(jq -n --arg val "$AUTH_TOKEN" '{ANTHROPIC_AUTH_TOKEN: $val}')

[ -n "$BASE_URL" ] && ENV_VARS=$(echo "$ENV_VARS" | jq --arg val "$BASE_URL" '. + {ANTHROPIC_BASE_URL: $val}')
[ -n "$CUSTOM_HEADERS" ] && ENV_VARS=$(echo "$ENV_VARS" | jq --arg val "$CUSTOM_HEADERS" '. + {ANTHROPIC_CUSTOM_HEADERS: $val}')

jq -n --argjson env "$ENV_VARS" '{env: $env}' > "$SETTINGS_OUTPUT"
chmod 600 "$SETTINGS_OUTPUT"

# Report what was configured
echo "Generated $SETTINGS_OUTPUT with environment variables:"
echo "  - ANTHROPIC_AUTH_TOKEN: set"
[ -n "$BASE_URL" ] && echo "  - ANTHROPIC_BASE_URL: $BASE_URL"
[ -n "$CUSTOM_HEADERS" ] && echo "  - ANTHROPIC_CUSTOM_HEADERS: set"

# GitHub CLI authentication (optional, captured from host by initializeCommand)
GH_TOKEN_FILE="$DEVCONTAINER_DIR/.gh-auth-token"
GH_TOKEN=$(cat "$GH_TOKEN_FILE" 2>/dev/null || echo "")
if [ -n "$GH_TOKEN" ]; then
    if command -v gh >/dev/null 2>&1; then
        if echo "$GH_TOKEN" | gh auth login --with-token 2>/dev/null; then
            echo "  - GitHub CLI: authenticated via host token"
        else
            echo "  - GitHub CLI: authentication failed (token may be invalid or expired)"
        fi
    else
        echo "  - GitHub CLI: host token captured but gh not installed in container"
    fi
else
    echo "  - GitHub CLI: no host token (gh not installed or not logged in on host)"
fi

