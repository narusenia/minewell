#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# Build a minewell toolchain from Minecraft's own data generator.
#
#     scripts/build-toolchain.sh 1.21.4 [out-dir]
#
# The command table is not embedded in the compiler on purpose (requirements section
# 1.2): it is data, and a new Minecraft version should be a data drop rather than a
# release of the compiler. This is where that data comes from — Mojang's generator,
# not a hand-written list.
#
# Leaves <out-dir>/<version>/{commands.json,toolchain.json}, which is the layout
# `mwlc::toolchain` reads.

set -euo pipefail

version="${1:?usage: build-toolchain.sh <minecraft version> [out-dir]}"
out="${2:-target/toolchains}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

manifest="https://launchermeta.mojang.com/mc/game/version_manifest_v2.json"

echo "resolving $version"
entry="$(curl -fsSL "$manifest" | jq -r --arg v "$version" '.versions[] | select(.id == $v) | .url')"
if [ -z "$entry" ]; then
  echo "no such Minecraft version: $version" >&2
  exit 1
fi

server="$(curl -fsSL "$entry" | jq -r '.downloads.server.url')"
echo "downloading server.jar"
curl -fsSL -o "$work/server.jar" "$server"

# The pack format lives in the jar rather than in the reports, and it is what a
# generated pack.mcmeta has to declare.
pack_format="$(unzip -p "$work/server.jar" version.json | jq -r '.pack_version.data')"
echo "pack_format $pack_format"

echo "running the data generator"
(cd "$work" && java -DbundlerMainClass=net.minecraft.data.Main -jar server.jar --reports >/dev/null)

commands="$work/generated/reports/commands.json"
if [ ! -f "$commands" ]; then
  echo "the generator produced no commands.json" >&2
  exit 1
fi

dir="$out/$version"
mkdir -p "$dir"
cp "$commands" "$dir/commands.json"
cat > "$dir/toolchain.json" <<JSON
{
  "pack_format": $pack_format,
  "minecraft_version": "$version"
}
JSON

echo "wrote $dir"
