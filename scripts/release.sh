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

# The release's own "released in vX.Y.Z" comments bump each shipped ticket's
# edit_time past the new tag's cutoff, so without this filter every shipped
# ticket would be recounted in the next release forever.
# ponytail: a ticket reopened after shipping never recounts (its old release
# comment sticks); use --bump/--version manually for that rare case.
if [ -n "$TICKET_LINES" ]; then
  UNSHIPPED=""
  while IFS= read -r line; do
    id=${line%%	*}
    if git-bug bug show "$id" 2>/dev/null | grep -q "released in v"; then
      continue
    fi
    UNSHIPPED="${UNSHIPPED}${UNSHIPPED:+$'\n'}${line}"
  done <<< "$TICKET_LINES"
  TICKET_LINES="$UNSHIPPED"
fi

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

# --- changelog ---------------------------------------------------------------
CHANGELOG=CHANGELOG.md

# Render one entry. Tickets carry the why and a lookup handle; commits catch
# work no ticket covered. A commit that belongs to a listed ticket says so
# ("Refs git-bug <id>"), so dedup is exact rather than guessed from prose.
changelog_entry() {
  local version="$1" prev="$2" upto="$3" date
  date=$(git log -1 --format=%cs "$upto" 2>/dev/null || date +%F)

  local features="" fixes="" id labels title
  if [ -n "$TICKET_LINES" ]; then
    while IFS=$'\t' read -r id labels title; do
      [ -n "$id" ] || continue
      if printf '%s' "$labels" | tr ',' '\n' | grep -qx feature; then
        features="${features}- ${title} (${id})"$'\n'
      else
        fixes="${fixes}- ${title} (${id})"$'\n'
      fi
    done <<< "$TICKET_LINES"
  fi

  # Everything else that landed. Merges and the bump commit carry nothing for
  # a reader; commits referencing a listed ticket are already described above.
  local range other="" sha subject
  range=$([ -n "$prev" ] && echo "$prev..$upto" || echo "$upto")
  while IFS=$'\t' read -r sha subject; do
    [ -n "$sha" ] || continue
    # Bookkeeping that means nothing to a reader of a changelog.
    case "$subject" in
      "Bump version to "*|"Point speckit at "*) continue ;;
    esac
    if [ -n "$TICKET_LINES" ] \
      && git show -s --format=%B "$sha" | grep -qE "git-bug ($(printf '%s' "$TICKET_LINES" | cut -f1 | paste -sd'|' -))"; then
      continue
    fi
    other="${other}- ${subject}"$'\n'
  done < <(git log --no-merges --format='%H%x09%s' "$range" 2>/dev/null)

  printf '## %s — %s\n' "$version" "$date"
  if [ -n "$features" ]; then printf '\n### Added\n\n%s' "$features"; fi
  if [ -n "$fixes$other" ]; then printf '\n### Fixed and changed\n\n%s%s' "$fixes" "$other"; fi
  if [ -z "$features$fixes$other" ]; then
    printf '\n_No tracked work recorded for this release — describe it here._\n'
  fi
}

# Prepend an entry, leaving everything already written untouched. Skipped when
# the version already has a section, so a resumed release cannot duplicate it.
write_changelog() {
  local version="$1" entry
  if [ -f "$CHANGELOG" ] && grep -q "^## $version " "$CHANGELOG"; then
    step "changelog already has $version"
    return
  fi
  entry=$(changelog_entry "$version" "$LAST_TAG" HEAD)
  step "write changelog entry for $version"
  {
    printf '# Changelog\n\n'
    printf '%s\n' "$entry"
    if [ -f "$CHANGELOG" ]; then
      tail -n +2 "$CHANGELOG" | sed '1{/^$/d;}'
    fi
  } > "$CHANGELOG.tmp"
  mv "$CHANGELOG.tmp" "$CHANGELOG"
}

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
  echo
  echo "changelog entry that would be written:"
  changelog_entry "$TAG" "$LAST_TAG" HEAD | sed 's/^/  /'
  echo
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

write_changelog "$TAG"

if [ -n "$(git status --porcelain)" ]; then
  step "commit version bump"
  git add Cargo.toml Cargo.lock "$CHANGELOG"
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

if gh release view "$TAG" --repo "$REPO" > /dev/null 2>&1; then
  step "github release $TAG already published"
else
  step "publish github release"
  if ! changelog_entry "$TAG" "$LAST_TAG" "$TAG" \
    | gh release create "$TAG" --repo "$REPO" --title "$TAG" --notes-file - > /dev/null; then
    echo "  WARNING: could not publish the github release; CHANGELOG.md is authoritative" >&2
  fi
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
    -e "s|assert_match \"shep [0-9.]*\"|assert_match \"shep $VERSION\"|")
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
