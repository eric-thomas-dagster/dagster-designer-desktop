#!/usr/bin/env bash
# Start a release when the product repo (dagster_designer) has commits that no release was built from.
#
# The release workflow builds the product repo's main branch, but only runs on pushes HERE, so a change that is only
# in the product repo (often just the web app) wouldn't ship. Run on a schedule, this closes the gap.
#
# It releases when ALL of these hold:
#   - the product repo's main differs from the commit the latest release says it was built from
#     (release notes end with "Built from dagster_designer@<sha>")
#   - no release run is queued or in progress
#   - the last release run didn't fail after that newest commit -- otherwise a broken build would retry forever;
#     it waits for a new commit instead
#
# Needs: gh (authenticated), GH_TOKEN with actions:write on this repo. Env: THIS_REPO, MAIN_REPO; DRY_RUN=1 to only report.
set -euo pipefail

THIS_REPO="${THIS_REPO:?set THIS_REPO, e.g. owner/dagster-designer-desktop}"
MAIN_REPO="${MAIN_REPO:?set MAIN_REPO, e.g. owner/dagster_designer}"

head_sha=$(gh api "repos/$MAIN_REPO/commits/main" --jq .sha)
head_date=$(gh api "repos/$MAIN_REPO/commits/main" --jq .commit.committer.date)

built=$(gh release view latest --repo "$THIS_REPO" --json body -q .body 2>/dev/null \
  | grep -o 'dagster_designer@[0-9a-f]\{7,40\}' | head -1 | cut -d@ -f2 || true)

if [ -n "$built" ] && [[ "$head_sha" == "$built"* ]]; then
  echo "Up to date: the latest release was built from ${built} (${MAIN_REPO} main)."
  exit 0
fi

active=$(gh run list --repo "$THIS_REPO" --workflow release.yml --json status \
  -q '[.[] | select(.status == "in_progress" or .status == "queued" or .status == "waiting")] | length')
if [ "$active" != "0" ]; then
  echo "A release is already running; checking again next time."
  exit 0
fi

read -r last_conclusion last_created < <(gh run list --repo "$THIS_REPO" --workflow release.yml --limit 1 \
  --json conclusion,createdAt -q '.[0] | "\(.conclusion) \(.createdAt)"' 2>/dev/null || echo "none 1970-01-01T00:00:00Z")
if [ "$last_conclusion" != "success" ] && [ "$last_conclusion" != "none" ] && [[ "$last_created" > "$head_date" ]]; then
  echo "The last release run ($last_conclusion, $last_created) came after ${head_sha:0:7} and did not succeed;"
  echo "not retrying until ${MAIN_REPO} gets a new commit."
  exit 0
fi

echo "Releasing: ${MAIN_REPO} main is ${head_sha:0:7}, the latest release was built from ${built:-nothing recorded}."
if [ "${DRY_RUN:-}" = "1" ]; then
  echo "(dry run: not starting it)"
else
  gh workflow run release.yml --repo "$THIS_REPO"
fi
