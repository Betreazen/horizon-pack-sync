#!/usr/bin/env bash
# Runs inside the image: exports a server engine from the mods in /mods into /out.
# BASE, BUILD_PROFILE and TRUST_FLAG come from publish.ps1, where the owner gave the consent.
set -euo pipefail

export HORIZON_LAUNCHER_HOME=/cache/launcher
launcher="$(find /opt/launcher -type f -name horizon-launcher -perm -u+x | head -n 1)"
version_dir="$HORIZON_LAUNCHER_HOME/versions/$BASE"

if [ ! -f "$version_dir/.installed" ]; then
    "$launcher" install "$BASE"
fi

# Preparing needs a signed-in account; the owner's computer has already done it.
rm -rf "$version_dir/.local"
cp -r /prepared "$version_dir/.local"

rm -rf /tmp/engine
"$launcher" native-build /mods --server-output /tmp/engine --version "$BASE" \
    --profile "$BUILD_PROFILE" "$TRUST_FLAG"

# A dedicated server runs on the engine and the prepared game files (.local/horizon-client*). Left
# out: Mojang's jars and libraries and the datagen run, which only prepare those files again; the
# sounds, since the server engine is built without audio; the source tree's test tools; and temp
# files of an interrupted write. Mojang's files then never reach a release either.
rm -rf /tmp/engine/tools /tmp/engine/.local/minecraft /tmp/engine/.local/datagen
find /tmp/engine/.local -type d -path '*/assets/minecraft/sounds' -prune -exec rm -rf {} +
find /tmp/engine -type f -name '*.tmp' -delete

tar -C /tmp/engine -czf /out/server-linux-x64.tar.gz .
