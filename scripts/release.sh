#!/usr/bin/env bash
# Releases borg from this machine, as libpetri does: sets the version, dates its CHANGELOG section,
# commits, runs scripts/check.sh, publishes the crates to crates.io, then tags vX.Y.Z, pushes and
# creates a draft GitHub release with that section as its notes. Pushing the tag publishes nothing to
# crates.io; the release workflow builds the tarballs scripts/install.sh fetches, attaches them to the
# draft and publishes it. A tag pushed ahead of the release (on the commit that is HEAD) is kept: the
# script then publishes and creates the draft, and re-runs the workflow for that tag.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DRY_RUN=false
VERSION=""

usage() {
    cat <<EOF
Usage: $(basename "$0") [--dry-run] <version>

Release borg-trace, borg-ai, borg-mesh and borg-web to crates.io and GitHub.

Prerequisites:
  - cargo login (crates.io token in ~/.cargo/credentials.toml)
  - gh CLI authenticated (for the GitHub release)
  - a CHANGELOG.md section "## <version> (unreleased)"

Options:
  --dry-run     Set the version and run the gate only; no commit, publish, tag or release
  -h, --help    Show this help

Example:
  $(basename "$0") --dry-run 0.6.0
  $(basename "$0") 0.6.0
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run) DRY_RUN=true; shift ;;
        -h|--help) usage; exit 0 ;;
        -*) echo "Unknown option: $1" >&2; usage >&2; exit 1 ;;
        *) VERSION="$1"; shift ;;
    esac
done

info()  { echo "==> $*"; }
error() { echo "Error: $*" >&2; exit 1; }

[[ -n "$VERSION" ]] || { usage >&2; exit 1; }
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || error "version must be X.Y.Z, got $VERSION"
TAG="v$VERSION"

# The CHANGELOG section for VERSION, without its heading: everything up to the next "## ".
changelog_section() {
    awk -v v="$VERSION" '
        /^## / { if (p) exit; if ($2 == v) p = 1; next }
        p
    ' "$ROOT/CHANGELOG.md"
}

info "Validating prerequisites"
cd "$ROOT"
if ! git diff --quiet || ! git diff --cached --quiet; then
    error "the working tree has uncommitted changes. Commit or stash first."
fi
[[ "$(git rev-parse --abbrev-ref HEAD)" == main ]] || error "release from main"
git fetch -q origin main
[[ "$(git rev-parse HEAD)" == "$(git rev-parse origin/main)" ]] || error "main is not origin/main; pull or push first"
grep -q "^## $VERSION " CHANGELOG.md || error "CHANGELOG.md has no \"## $VERSION\" section"
if [[ "$DRY_RUN" == false ]]; then
    [[ -f ~/.cargo/credentials.toml ]] || error "no cargo credentials. Run 'cargo login' first."
    gh auth status >/dev/null 2>&1 || error "GitHub CLI not authenticated. Run 'gh auth login' first."
    # The release is created after the crates are published, so check the right to create it first.
    perm=$(gh repo view --json viewerPermission --jq .viewerPermission 2>/dev/null || true)
    [[ "$perm" =~ ^(ADMIN|MAINTAIN|WRITE)$ ]] \
        || error "the active gh account cannot create releases here (permission: ${perm:-none}); gh auth switch"
fi
# A tag pushed before the release must be on HEAD: it is the commit the crates are published from.
TAGGED=false
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    [[ "$(git rev-parse "$TAG^{commit}")" == "$(git rev-parse HEAD)" ]] || error "tag $TAG exists and is not HEAD"
    git ls-remote --exit-code --tags origin "$TAG" >/dev/null || error "tag $TAG is not pushed; git push origin $TAG"
    TAGGED=true
elif git ls-remote --exit-code --tags origin "$TAG" >/dev/null; then
    error "tag $TAG exists on origin only; git fetch --tags"
fi
if [[ "$DRY_RUN" == false && "$TAGGED" == true ]] && gh release view "$TAG" >/dev/null 2>&1; then
    error "a GitHub release for $TAG already exists"
fi

info "Setting the version to $VERSION"
# The workspace version, which every crate inherits, and each "=X.Y.Z" pin on a workspace crate.
sed -i "/^\[workspace.package\]/,/^\[/ s/^version = \"[^\"]*\"/version = \"$VERSION\"/" Cargo.toml
sed -i -E "s/(path = \"[^\"]*\", version = \")=[0-9]+\.[0-9]+\.[0-9]+\"/\1=$VERSION\"/" crates/*/Cargo.toml
cargo update -w -q
bad=$(cargo metadata --format-version 1 --no-deps | jq -r --arg v "$VERSION" \
    '.packages[] | select(.version != $v) | "\(.name) \(.version)"')
[[ -z "$bad" ]] || error "crates not at $VERSION: $bad"
if [[ "$TAGGED" == true ]] && ! git diff --quiet; then
    error "tag $TAG is not at version $VERSION"
fi

info "Running scripts/check.sh"
scripts/check.sh

if [[ "$DRY_RUN" == true ]]; then
    info "Dry run complete: the gate passed for $VERSION."
    git diff --quiet || info "The version change is left uncommitted; 'git checkout -- .' undoes it."
    exit 0
fi

if [[ "$TAGGED" == false ]]; then
    info "Dating the CHANGELOG section and committing"
    sed -i "s/^## $VERSION (unreleased)$/## $VERSION ($(date +%Y-%m-%d))/" CHANGELOG.md
    git add Cargo.toml Cargo.lock crates/*/Cargo.toml CHANGELOG.md
    git diff --cached --quiet || git commit -q -m "release: $VERSION"
fi

# cargo publish --workspace publishes in dependency order, waits for each crate to be indexed and
# skips the publish = false crates (borg-eval, borg-search). crates.io refuses a version it already
# has, so a re-run after a partial publish excludes the crates the sparse index lists.
info "Publishing to crates.io"
exclude=() left=0
for name in $(cargo metadata --format-version 1 --no-deps | jq -r '.packages[] | select(.publish != []) | .name'); do
    if curl -fsS "https://index.crates.io/${name:0:2}/${name:2:2}/$name" 2>/dev/null \
        | jq -e --arg v "$VERSION" 'select(.vers == $v)' >/dev/null; then
        info "$name $VERSION is already on crates.io, skipping"
        exclude+=(--exclude "$name")
    else
        left=$((left + 1))
    fi
done
if [[ "$left" -gt 0 ]]; then
    cargo publish --workspace --locked "${exclude[@]}"
fi

if [[ "$TAGGED" == false ]]; then
    info "Tagging $TAG and pushing"
    git tag -a "$TAG" -m "Release $VERSION"
    git push -q origin main
    git push -q origin "$TAG"
fi

info "Creating the draft GitHub release"
changelog_section | gh release create "$TAG" --draft --verify-tag --title "borg $VERSION" --notes-file -
# The tag's own run found no draft to attach to; this run, on the tag, does.
if [[ "$TAGGED" == true ]]; then
    gh workflow run release.yml --ref "$TAG"
fi

info "Released $VERSION to crates.io. The release workflow attaches the tarballs and publishes the release:"
info "  gh run watch \$(gh run list --workflow release.yml --limit 1 --json databaseId --jq '.[0].databaseId')"
