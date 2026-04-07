#!/usr/bin/env bash
set -euo pipefail

# Get current version from Cargo.toml
CURRENT_VERSION=$(grep '^version = ' Cargo.toml | head -n 1 | sed 's/version = "\(.*\)"/\1/')

if [ -z "$CURRENT_VERSION" ]; then
    echo "error: could not find version in Cargo.toml"
    exit 1
fi

BRANCH=$(git rev-parse --abbrev-ref HEAD)
REMOTE=$(git config "branch.${BRANCH}.remote" 2>/dev/null || echo origin)

echo "Current version: $CURRENT_VERSION"

# Determine new version
if [ $# -eq 0 ]; then
    # No argument: increment patch version
    IFS='.' read -r major minor patch <<< "$CURRENT_VERSION"
    NEW_VERSION="$major.$minor.$((patch + 1))"
    echo "Incrementing patch version to: $NEW_VERSION"
else
    # Argument given: use it (strip 'v' prefix if present)
    NEW_VERSION="${1#v}"

    # Validate version format
    if ! [[ "$NEW_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        echo "error: invalid version format '$NEW_VERSION' (expected X.Y.Z)"
        exit 1
    fi

    echo "Setting version to: $NEW_VERSION"
fi

# Check if version already exists as a tag (local + all remotes)
TAG_EXISTS=false
LOCAL_TAG=$(git rev-parse --verify "refs/tags/v$NEW_VERSION" 2>/dev/null || true)
REMOTES_WITH_TAG=()
for r in $(git remote); do
    if git ls-remote --tags "$r" "refs/tags/v$NEW_VERSION" 2>/dev/null | grep -q .; then
        REMOTES_WITH_TAG+=("$r")
    fi
done

if [ -n "$LOCAL_TAG" ] || [ ${#REMOTES_WITH_TAG[@]} -gt 0 ]; then
    TAG_EXISTS=true
    if [ "$CURRENT_VERSION" != "$NEW_VERSION" ]; then
        echo "error: tag v$NEW_VERSION exists but Cargo.toml is at $CURRENT_VERSION"
        echo "  (version mismatch — something is off)"
        exit 1
    fi

    echo "Tag v$NEW_VERSION already exists:"
    [ -n "$LOCAL_TAG" ] && echo "  local:  $LOCAL_TAG"
    for r in "${REMOTES_WITH_TAG[@]}"; do
        echo "  remote: $r"
    done

    read -ep "Delete and re-tag at HEAD? [y/N] " YN
    if [[ ! "$YN" =~ [Yy] ]]
    then exit 0
    fi

    # Nuke the old tag everywhere, then fall through to normal tagging
    [ -n "$LOCAL_TAG" ] && git tag -d "v$NEW_VERSION"
    for r in "${REMOTES_WITH_TAG[@]}"; do
        echo "Removing tag from $r..."
        git push "$r" ":refs/tags/v$NEW_VERSION"
    done
fi

if [ "$CURRENT_VERSION" != "$NEW_VERSION" ]; then
    # Update Cargo.toml
    echo "Updating Cargo.toml..."
    sed -i "0,/^version = /s/^version = \".*\"/version = \"$NEW_VERSION\"/" Cargo.toml

    # Verify the change
    UPDATED_VERSION=$(grep '^version = ' Cargo.toml | head -n 1 | sed 's/version = "\(.*\)"/\1/')
    if [ "$UPDATED_VERSION" != "$NEW_VERSION" ]; then
        echo "error: failed to update Cargo.toml (got $UPDATED_VERSION, expected $NEW_VERSION)"
        exit 1
    fi

    # Update Cargo.lock
    echo "Updating Cargo.lock..."
    cargo generate-lockfile

    # Commit the version bump
    echo "Committing version bump..."
    git add Cargo.toml Cargo.lock
    git commit -m "Bump version to $NEW_VERSION"
fi

# Create signed tag
echo "Creating signed tag v$NEW_VERSION..."
git tag -s "v$NEW_VERSION" -m "Release version $NEW_VERSION"

rm -vf target/*/appserv 2>/dev/null

echo ""
echo "Success! Version bumped to $NEW_VERSION and tagged as v$NEW_VERSION"
echo

read -ep "publish the new version (git push / push --tags)? [Y/n] " YN
if [[ ! "$YN" =~ [Nn] ]]; then
    if ! (git push && git push --tags); then
        echo "Push failed, rolling back version bump..."
        git tag -d "v$NEW_VERSION"
        if [ "$CURRENT_VERSION" != "$NEW_VERSION" ]; then
            git reset HEAD~1
            git checkout Cargo.toml Cargo.lock
        fi
        echo "Rollback complete."
        exit 1
    fi
fi
