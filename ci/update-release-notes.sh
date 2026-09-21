#!/usr/bin/env bash
# Publishes the prepared release: renders the English downloads table plus the
# "What's Changed" commit list, then refreshes the release notes (prerelease
# flag kept from prepare).
#
# Commit collection scope:
#   prerelease (v<ver>-dev.<n>)  -> commits since the previous tag of EITHER
#                                   kind — the nearest dev or release tag by
#                                   semantic version (a release outranks its
#                                   own dev tags). Right after a release the
#                                   first dev of the new cycle thus collects
#                                   everything since that release, while
#                                   later devs stay incremental.
#   release    (v<ver>)          -> commits since the previous release tag
#                                   (strict vX.Y.Z only; the newest dev tag
#                                   usually sits on the same commit and
#                                   would render "What's Changed" blank)
set -euo pipefail

RELEASE_ID="${1:?release id}"
TAG="${2:?tag}"
PRERELEASE="${3:?prerelease}"
REPO="${GITHUB_REPOSITORY:?}"
GH_TOKEN="${GH_TOKEN:?}"

# All uploaded assets; fail early if any name does not match the classifier.
# Fetch by numeric release id (REST) — `gh release view` resolves tag -> node_id
# for draft releases, and tag lookup 404s on untagged drafts.
ASSETS_JSON="$(gh api "repos/$REPO/releases/$RELEASE_ID" --jq '[.assets[].name]' | jq -c .)"

# Commit collection is shared with the initial release body (prepare job runs
# it BEFORE the release is created); see ci/collect-commits.sh.
./ci/collect-commits.sh "$TAG" commits.txt

node ci/release-notes-render.mjs "$TAG" "$ASSETS_JSON" commits.txt > notes.md

# Release was created published by resolve-release.sh; just refresh the notes.
# (Keeps the prerelease flag that prepare resolved.)
# REST PATCH by numeric id (like the GugleFS pipeline): `gh release edit`
# resolves its argument as a TAG, and a bare numeric id is not a tag, so it
# fails with "release not found" even though the release exists.
gh api -X PATCH "repos/$REPO/releases/$RELEASE_ID" \
  -F body=@notes.md \
  -F prerelease="$PRERELEASE" \
  --silent
