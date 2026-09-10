#!/usr/bin/env bash
# Release shepherd: bump, tag, push, update the homebrew tap, comment tickets.
# Policy and usage: docs/RELEASING.md. Every step is idempotency-probed, so
# re-running after a failure resumes where it stopped.
set -euo pipefail

REPO=dmitrym0/shepherd
TAP_REPO=dmitrym0/homebrew-tap
FORMULA_PATH=Formula/shep.rb

VERSION="" BUMP="" YES=0 FORCE=0 DRY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --bump) BUMP="$2"; shift 2 ;;
    --yes) YES=1; shift ;;
    --force) FORCE=1; shift ;;
    --dry-run) DRY=1; shift ;;
    -h|--help)
      echo "usage: release.sh [--version X.Y.Z | --bump minor|patch|major] [--yes] [--force] [--dry-run]"
      exit 0 ;;
    *) echo "release: unknown flag: $1" >&2; exit 2 ;;
  esac
done

die() { echo "release: $*" >&2; exit 1; }
step() { echo "==> $*"; }

case "$BUMP" in ""|minor|patch|major) ;; *) die "--bump must be minor, patch or major" ;; esac

# --- refusals (never overridable; fix the cause instead) ---------------------
# A dry run changes nothing, so refusals soften to warnings there.
refuse() { if [ "$DRY" = 1 ]; then echo "warning (would refuse): $*" >&2; else die "$*"; fi }
[ -z "$(git status --porcelain)" ] || refuse "working tree is dirty; commit or stash first"
[ "$(git branch --show-current)" = "main" ] || refuse "releases run from main"

# --- last release and tickets closed since ----------------------------------
LAST_TAG=$(git describe --tags --abbrev=0 --match 'v*' 2>/dev/null || true)
if [ -z "$LAST_TAG" ]; then
  [ -n "$VERSION" ] || die "no previous v* tag; first release needs an explicit --version"
  CUTOFF="1970-01-01T00:00:00+00:00"
else
  CUTOFF=$(git log -1 --format=%cI "$LAST_TAG")
fi

# Closed git-bug tickets edited after the cutoff (edit time approximates close
# time; see specs/004-release-infrastructure/research.md D2).
TICKET_LINES=$(git-bug bug --format json status:closed 2>/dev/null | python3 -c '
import json, sys
from datetime import datetime
cutoff = datetime.fromisoformat(sys.argv[1])
for bug in json.load(sys.stdin):
    edited = datetime.fromisoformat(bug["edit_time"]["time"])
    if edited > cutoff:
        labels = ",".join(bug.get("labels") or []) or "-"
        print("%s\t%s\t%s" % (bug["id"][:7], labels, bug["title"]))
' "$CUTOFF" || true)

# --- pick the version --------------------------------------------------------
if [ -z "$VERSION" ] && [ -z "$BUMP" ]; then
  if [ -z "$TICKET_LINES" ]; then
    [ "$FORCE" = 1 ] || die "no tickets closed since $LAST_TAG — nothing to release (--force to override)"
    BUMP="patch"
  elif printf '%s\n' "$TICKET_LINES" | cut -f2 | tr ',' '\n' | grep -qx feature; then
    BUMP=minor
  else
    BUMP="patch"
  fi
fi
if [ -z "$VERSION" ]; then
  BASE=${LAST_TAG#v}
  MAJOR=${BASE%%.*}; REST=${BASE#*.}; MINOR=${REST%%.*}; PATCH=${REST#*.}
  case "$BUMP" in
    major) VERSION="$((MAJOR + 1)).0.0" ;;
    minor) VERSION="$MAJOR.$((MINOR + 1)).0" ;;
    patch) VERSION="$MAJOR.$MINOR.$((PATCH + 1))" ;;
  esac
fi
echo "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || die "not a version: $VERSION"
TAG="v$VERSION"
TARBALL_URL="https://github.com/$REPO/archive/refs/tags/$TAG.tar.gz"

# --- show the plan, confirm --------------------------------------------------
echo "release $LAST_TAG -> $TAG"
if [ -n "$TICKET_LINES" ]; then
  echo "tickets shipped:"
  printf '%s\n' "$TICKET_LINES" | while IFS=$'\t' read -r id labels title; do
    echo "  $id  [$labels]  $title"
  done
else
  echo "tickets shipped: none (manual/forced release)"
fi
echo "steps: bump Cargo.toml, commit, tag $TAG, push, update $TAP_REPO/$FORMULA_PATH, comment tickets"
if [ "$DRY" = 1 ]; then
  echo "dry run: no changes made"
  exit 0
fi
if [ "$YES" != 1 ]; then
  printf 'proceed? [y/N] '
  read -r answer
  [ "$answer" = y ] || [ "$answer" = Y ] || die "aborted"
fi

# --- release steps, each with an idempotency probe ---------------------------
if grep -q "^version = \"$VERSION\"" Cargo.toml; then
  step "Cargo.toml already at $VERSION"
else
  step "bump Cargo.toml to $VERSION"
  sed -i '' "s/^version = \".*\"/version = \"$VERSION\"/" Cargo.toml
  cargo build --quiet   # refresh Cargo.lock
fi

if [ -n "$(git status --porcelain)" ]; then
  step "commit version bump"
  git add Cargo.toml Cargo.lock
  git commit -q -m "Bump version to $VERSION"
else
  step "bump already committed"
fi

if git rev-parse -q --verify "refs/tags/$TAG" > /dev/null; then
  step "tag $TAG already exists"
else
  step "tag $TAG"
  git tag "$TAG"
fi

if git ls-remote --tags origin "refs/tags/$TAG" | grep -q .; then
  step "tag already on origin"
else
  step "push main and $TAG"
  git push -q origin main "$TAG"
fi

step "compute tarball sha256"
SHA=""
for _ in 1 2 3 4 5; do
  SHA=$(curl -fsL "$TARBALL_URL" | shasum -a 256 | cut -d' ' -f1) && [ -n "$SHA" ] && break
  sleep 3
done
[ -n "$SHA" ] || die "could not fetch $TARBALL_URL"

step "update tap formula"
FORMULA_JSON=$(gh api "repos/$TAP_REPO/contents/$FORMULA_PATH")
PARENT_SHA=$(printf '%s' "$FORMULA_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin)["sha"])')
OLD_FORMULA=$(printf '%s' "$FORMULA_JSON" | python3 -c 'import base64,json,sys; sys.stdout.write(base64.b64decode(json.load(sys.stdin)["content"]).decode())')
if printf '%s' "$OLD_FORMULA" | grep -q "$SHA" && printf '%s' "$OLD_FORMULA" | grep -q "$TAG.tar.gz"; then
  step "formula already at $VERSION"
else
  NEW_FORMULA=$(printf '%s' "$OLD_FORMULA" | sed \
    -e "s|url \".*\"|url \"$TARBALL_URL\"|" \
    -e "s|sha256 \".*\"|sha256 \"$SHA\"|" \
    -e "s|assert_match \"shep .*\"|assert_match \"shep $VERSION\"|")
  diff <(printf '%s\n' "$OLD_FORMULA") <(printf '%s\n' "$NEW_FORMULA") || true
  printf '%s' "$NEW_FORMULA" | gh api -X PUT "repos/$TAP_REPO/contents/$FORMULA_PATH" \
    -f message="shep $VERSION" \
    -f sha="$PARENT_SHA" \
    -f content="$(printf '%s' "$NEW_FORMULA" | base64)" > /dev/null
fi

step "verify published formula checksum"
PUBLISHED_SHA=$(gh api "repos/$TAP_REPO/contents/$FORMULA_PATH" \
  | python3 -c 'import base64,json,sys; sys.stdout.write(base64.b64decode(json.load(sys.stdin)["content"]).decode())' \
  | sed -n 's/.*sha256 "\(.*\)".*/\1/p')
[ "$PUBLISHED_SHA" = "$SHA" ] || die "published formula sha256 ($PUBLISHED_SHA) != tarball sha256 ($SHA)"

if [ -n "$TICKET_LINES" ]; then
  step "comment shipped tickets"
  printf '%s\n' "$TICKET_LINES" | while IFS=$'\t' read -r id labels title; do
    if git-bug bug show "$id" 2>/dev/null | grep -q "released in $TAG"; then
      echo "  $id already commented"
    else
      git-bug bug comment new "$id" --non-interactive -m "released in $TAG" > /dev/null
      echo "  $id commented"
    fi
  done
fi

echo "released $TAG — brew upgrade shep to verify installation"
