#!/usr/bin/env bash
# Rewrite the commits a PR branch adds on top of origin/main so they carry only
# the owner's name: author and committer set, attribution trailers dropped.
# The tree is checked to be byte-identical afterwards, so only metadata moves.
#
# Usage: rewrite_branch.sh <worktree-dir> <local-branch>
# Override the identity with OWNER_NAME / OWNER_EMAIL if it ever changes.
set -euo pipefail
dir="$1"
branch="$2"
name="${OWNER_NAME:-Vitali Avagyan}"
email="${OWNER_EMAIL:-eheva87@gmail.com}"
here="$(cd "$(dirname "$0")" && pwd)"
cd "$dir"
git fetch -q origin main
if [ "$(git rev-list --count "origin/main..$branch")" -eq 0 ]; then
  echo "$branch: no commits beyond origin/main, nothing to rewrite"
  exit 0
fi
old="$(git rev-parse "$branch")"
old_tree="$(git rev-parse "$branch^{tree}")"
OWNER_NAME="$name" OWNER_EMAIL="$email" FILTER_BRANCH_SQUELCH_WARNING=1 git filter-branch -f \
  --env-filter '
    export GIT_AUTHOR_NAME="$OWNER_NAME" GIT_AUTHOR_EMAIL="$OWNER_EMAIL"
    export GIT_COMMITTER_NAME="$OWNER_NAME" GIT_COMMITTER_EMAIL="$OWNER_EMAIL"
  ' \
  --msg-filter "python3 -I '$here/strip_attribution.py'" \
  -- "origin/main..$branch" >/dev/null
new="$(git rev-parse "$branch")"
[ "$(git rev-parse "$branch^{tree}")" = "$old_tree" ] \
  || { echo "TREE CHANGED for $branch" >&2; exit 1; }
echo "$branch: $old -> $new (tree identical)"
echo "authors/committers in origin/main..$branch:"
git log "origin/main..$branch" --format='  %an <%ae> / %cn <%ce>' | sort | uniq -c
echo "message lines still mentioning claude:"
git log "origin/main..$branch" --format='%B' | grep -in 'claude\|co-authored' || echo "  none"
