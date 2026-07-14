#!/usr/bin/env bash
# kill-test-servers.sh — stop this worktree's Thunderduck + Spark test servers.
#
# Worktree-scoped and ownership-verified: it only signals a listener after
# proving the process belongs to a Thunderduck test run for the target worktree
# (Thunderduck binary under <worktree>/target/, or a Spark JVM stamped with
# -Dthunderduck.worktree=<id>). It will NEVER kill another worktree's or another
# dev session's servers — use this instead of `pkill -f thunderduck-connect-server`.
#
# Usage:
#   ./tests/scripts/kill-test-servers.sh              # kill THIS worktree's servers
#   ./tests/scripts/kill-test-servers.sh --stale      # only reap orphaned (ppid==1) servers here
#   ./tests/scripts/kill-test-servers.sh --all         # kill every worktree's servers (still ownership-verified)
#   ./tests/scripts/kill-test-servers.sh --all --stale # reap orphaned servers across all worktrees
#   ./tests/scripts/kill-test-servers.sh --list        # show this worktree's servers
#   ./tests/scripts/kill-test-servers.sh --list-all    # show every worktree's servers
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_ENV="$SCRIPT_DIR/../integration/utils/test_env.py"

STALE=""
MODE="--kill"
for arg in "$@"; do
    case "$arg" in
        --stale)     STALE="--stale" ;;
        --all)       MODE="--kill-all" ;;
        --list)      MODE="--list" ;;
        --list-all)  MODE="--list-all" ;;
        -h|--help)   sed -n '2,20p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown arg: $arg" >&2; exit 2 ;;
    esac
done

exec python3 "$TEST_ENV" "$MODE" $STALE
