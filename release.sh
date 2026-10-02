#!/bin/sh
# Builds, signs and publishes dist/ as the GitHub release for the version in Cargo.toml, which is
# where the app's update check looks. Run from a clean tree, so the files match the tagged code.
set -eu
cd "$(dirname "$0")"
repo=PedroElizalde01/yunta
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
tag="v$version"

# Untracked files never reach the build, so only changes to tracked ones count.
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
    echo "Commit or stash your changes first: a release is built from a clean tree." >&2
    exit 1
fi
tagged=no
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    # The tag may sit on an earlier commit, as long as nothing that goes into the build changed.
    if ! git diff --quiet "$tag" HEAD -- src assets build.rs .cargo Cargo.toml Cargo.lock package.sh; then
        echo "$tag exists and the code changed since: raise the version in Cargo.toml." >&2
        exit 1
    fi
    tagged=yes
fi

# Everything is built, tested, signed and checked before anything is published, so a failure
# here leaves no tag or release behind.
cargo test --release
./package.sh
key=${YUNTA_SIGNING_KEY:-$HOME/.config/yunta-release/signing.pem}
if [ ! -f dist/SHA256SUMS.sig ]; then
    echo "dist/ is not signed, so the app would refuse it as an update: not releasing." >&2
    exit 1
fi
(cd dist && sha256sum --quiet -c SHA256SUMS)
openssl pkey -in "$key" -pubout | openssl pkeyutl -verify -pubin -inkey /dev/stdin -rawin -in dist/SHA256SUMS -sigfile dist/SHA256SUMS.sig

if [ "$tagged" = no ]; then
    git tag -a "$tag" -m "Yunta $version"
fi
git push origin HEAD "$tag"

# A token for this repo alone, if there is one; gh's own login otherwise.
token=$HOME/.config/yunta-release/gh-token
if [ -f "$token" ]; then
    GH_TOKEN=$(cat "$token")
    export GH_TOKEN
fi
notes="Linux (X11, Debian and Ubuntu based): \`sudo apt install ./yunta_${version}_amd64.deb\`
Windows 10 and 11: \`yunta.exe\` is the whole app; put it anywhere and run it.

SHA256SUMS lists both files and is signed with the project's Ed25519 key (SHA256SUMS.sig).
Yunta's one-click update checks that signature before installing anything."
if gh release view "$tag" -R "$repo" >/dev/null 2>&1; then
    gh release upload "$tag" dist/* -R "$repo" --clobber
else
    gh release create "$tag" dist/* -R "$repo" --verify-tag --title "Yunta $version" --notes "$notes"
fi
