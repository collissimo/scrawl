#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
version=2.9.4
sha256=ce89daf967db1e1893ed3ebd67575ed82d3902563e3191ca92aaec9164fbdef9
# SCRAWL_SPARKLE_DIR keeps the tools outside target/ (which cleans wipe), e.g.
# ~/.local/share/scrawl-release/sparkle-2.9.4 on the Macs that release (docs/release.md).
destination="${SCRAWL_SPARKLE_DIR:-$root/target/sparkle-$version}"
parent=$(dirname -- "$destination")
archive="$parent/Sparkle-$version.tar.xz"
url="https://github.com/sparkle-project/Sparkle/releases/download/$version/Sparkle-$version.tar.xz"

if [ -x "$destination/bin/generate_appcast" ] \
  && [ -x "$destination/bin/sign_update" ] \
  && [ -d "$destination/Sparkle.framework" ]; then
  echo "$destination"
  exit 0
fi

for cache in "$root/target/sparkle-$version" "$HOME/code/open-source/voice-control/target/sparkle-$version"; do
  if [ "$cache" != "$destination" ] && [ -d "$cache/Sparkle.framework" ] && [ -x "$cache/bin/generate_appcast" ]; then
    mkdir -p "$parent"
    rm -rf "$destination.partial"
    /usr/bin/ditto "$cache" "$destination.partial"
    mv "$destination.partial" "$destination"
    echo "$destination"
    exit 0
  fi
done

mkdir -p "$parent"
curl --fail --location --retry 3 --continue-at - --output "$archive.partial" "$url"
if ! printf '%s  %s\n' "$sha256" "$archive.partial" | shasum -a 256 -c - >&2; then
  rm -f "$archive.partial"
  exit 1
fi
mv "$archive.partial" "$archive"
rm -rf "$destination.partial"
mkdir -p "$destination.partial"
tar -xJf "$archive" -C "$destination.partial"
mv "$destination.partial" "$destination"
echo "$destination"
