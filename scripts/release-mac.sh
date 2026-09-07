#!/usr/bin/env bash
# Cut a distributable Setbuddy.app.
#
#   scripts/release-mac.sh [--publish]
#
# Universal build -> Developer ID signature -> notarisation -> stapled ticket ->
# zip -> sha256. With --publish it also creates the GitHub release the cask
# points at. Without it, everything is built and verified and nothing leaves the
# machine.
#
# One-time setup, interactive, so it is not done here:
#   xcrun notarytool store-credentials setbuddy \
#       --apple-id <your Apple ID> --team-id 34VGHNCG6J --password <app-specific>
# The app-specific password comes from appleid.apple.com, not your Apple ID
# password. Override the profile name with SETBUDDY_NOTARY_PROFILE.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="$ROOT/target/Setbuddy.app"
DIST="$ROOT/target/dist"
NOTARY_PROFILE="${SETBUDDY_NOTARY_PROFILE:-setbuddy}"
PUBLISH=0
[ "${1:-}" = "--publish" ] && PUBLISH=1

VERSION="$(awk -F'"' '/^version = /{print $2; exit}' "$ROOT/Cargo.toml")"
ZIP="$DIST/Setbuddy-$VERSION.zip"

# A release has to be rebuildable from what is on the remote, so it is cut from
# a clean tree. Nothing here is reversible once the zip has a sha256 in a cask.
if [ -n "$(git -C "$ROOT" status --porcelain)" ]; then
    echo "release: working tree is dirty; commit or stash first" >&2
    exit 1
fi

# The identity is looked up rather than hard-coded: whoever holds the Developer
# ID for this team can cut a release without editing the script.
if [ -z "${SETBUDDY_SIGN_IDENTITY:-}" ]; then
    SETBUDDY_SIGN_IDENTITY="$(security find-identity -v -p codesigning |
        sed -n 's/.*"\(Developer ID Application: .*\)"/\1/p' | head -1)"
fi
if [ -z "$SETBUDDY_SIGN_IDENTITY" ]; then
    echo "release: no Developer ID Application identity in the keychain." >&2
    echo "  An ad-hoc signature cannot be notarised, and Homebrew quarantines" >&2
    echo "  what it downloads, so the app would be refused on every other Mac." >&2
    exit 1
fi
echo "==> signing as: $SETBUDDY_SIGN_IDENTITY"

export SETBUDDY_SIGN_IDENTITY
"$ROOT/scripts/build-mac-app.sh" universal

echo "==> verifying the signature before spending a notarisation on it"
codesign --verify --deep --strict --verbose=2 "$APP"

rm -rf "$DIST"
mkdir -p "$DIST"
# ditto, not zip: the signature lives in extended attributes and resource forks
# that `zip` drops, which notarisation then rejects.
echo "==> packaging $ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"

echo "==> notarising (a few minutes; Apple scans every Mach-O in the bundle)"
xcrun notarytool submit "$ZIP" --keychain-profile "$NOTARY_PROFILE" --wait

# Stapling writes the ticket into the bundle so Gatekeeper clears it offline.
# The zip made above was only a carrier for the upload; the shipped one has to
# be made again, after the staple, or users get the unstapled bundle.
echo "==> stapling"
xcrun stapler staple "$APP"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"

echo "==> Gatekeeper verdict on the stapled bundle"
spctl --assess --type execute --verbose=2 "$APP"

SHA="$(shasum -a 256 "$ZIP" | cut -d' ' -f1)"
echo "$SHA  $(basename "$ZIP")" > "$ZIP.sha256"

echo
echo "==> $ZIP"
echo "    version  $VERSION"
echo "    archs    $(lipo -archs "$APP/Contents/MacOS/Setbuddy")"
echo "    sha256   $SHA"

if [ "$PUBLISH" = 1 ]; then
    echo "==> publishing v$VERSION"
    gh release create "v$VERSION" "$ZIP" \
        --title "Setbuddy $VERSION" \
        --notes "Universal (Apple silicon and Intel), signed and notarised.

Install:

    brew install --cask bongofongo/setbuddy/setbuddy

Requires mpv (\`brew install mpv\`), which the cask pulls in."
    # Closing the loop by hand is how a cask ends up pointing at a digest that
    # is one release out of date. SETBUDDY_TAP is a checkout of the tap repo.
    if [ -n "${SETBUDDY_TAP:-}" ]; then
        CASK="$SETBUDDY_TAP/Casks/setbuddy.rb"
        echo "==> updating $CASK"
        /usr/bin/sed -i '' \
            -e "s|^  version \".*\"|  version \"$VERSION\"|" \
            -e "s|^  sha256 \".*\"|  sha256 \"$SHA\"|" "$CASK"
        git -C "$SETBUDDY_TAP" commit -q -am "setbuddy $VERSION"
        git -C "$SETBUDDY_TAP" push -q
        echo "==> cask updated and pushed"
    else
        echo "==> update the cask by hand: version $VERSION, sha256 $SHA"
        echo "    (or set SETBUDDY_TAP to a checkout of homebrew-setbuddy)"
    fi
fi
