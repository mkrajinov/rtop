#!/bin/sh
# Prints the next release tag from the Conventional Commits since the last v* tag,
# or nothing when no commit warrants a release.
#
#   feat:                      minor (v0.1.0 → v0.2.0)
#   fix:, perf:                patch (v0.1.0 → v0.1.1)
#   type!: / BREAKING CHANGE:  major (v1.2.3 → v2.0.0); before v1 it bumps minor
#   anything else              no release (docs, ci, build, test, chore, refactor, style)
set -e
cd "$(dirname "$0")/.."
last=$(git describe --tags --abbrev=0 --match 'v[0-9]*.[0-9]*.[0-9]*' 2>/dev/null || true)
range=HEAD
[ -n "$last" ] && range="$last..HEAD"

subjects=$(git log --no-merges --format=%s "$range")
bodies=$(git log --no-merges --format=%b "$range")
if printf '%s\n' "$subjects" | grep -qE '^[a-z]+(\([^)]*\))?!:' ||
	printf '%s\n' "$bodies" | grep -qE '^BREAKING[ -]CHANGE:'; then
	bump=major
elif printf '%s\n' "$subjects" | grep -qE '^feat(\([^)]*\))?:'; then
	bump=minor
elif printf '%s\n' "$subjects" | grep -qE '^(fix|perf)(\([^)]*\))?:'; then
	bump=patch
else
	exit 0
fi

v=${last#v}
[ -z "$v" ] && v=0.0.0
major=${v%%.*}
rest=${v#*.}
minor=${rest%%.*}
patch=${rest#*.}
case $bump in
major)
	if [ "$major" -eq 0 ]; then
		minor=$((minor + 1)) patch=0
	else
		major=$((major + 1)) minor=0 patch=0
	fi
	;;
minor) minor=$((minor + 1)) patch=0 ;;
patch) patch=$((patch + 1)) ;;
esac
echo "v$major.$minor.$patch"
