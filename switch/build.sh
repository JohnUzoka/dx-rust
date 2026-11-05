#!/usr/bin/env bash
# Builds DX RUST for the Nintendo Switch as an NRO, with bevy-horizon (github.com/JohnUzoka/bevy-horizon) checked
# out next to this repository and NVK built in it (its docs/building.md):
#
#   switch/build.sh   ->  target/aarch64-nintendo-horizon/release/dxview.nro, and the ELF dxview beside it
#
# BEVY_HORIZON points at another checkout. JOBS, VERSION, ICON, LTO and CARGO_ARGS are bevy-horizon's
# scripts/build_game.sh variables. Keep the ELF of every build you hand out: crash addresses are looked up in it.
#
# On the SD card, sdmc:/switch/dxview/ holds:
#   dxview.nro
#   launcher.txt   GamePath=sdmc:/switch/dxview/DeusEx
#   DeusEx/        System/, Maps/, Textures/, Sounds/ and Music/ from your own Deus Ex install
#   options.txt    optional: profiling and A/B switches (template: switch/options.txt)
# The game writes settings.txt, keybinds.txt, session.txt and Save/ there, bevy-horizon log.txt, panic.txt and,
# when profiling, cpu-profile.txt.
# Start it with a game's memory (a forwarder, or hold R while starting a game), not from the album.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bh="$(cd "${BEVY_HORIZON:-$repo/../bevy-horizon}" && pwd)"

# A patch in switch/patches.toml is used only while Cargo.lock locks its version. An unused one would build the
# crates.io crate, which builds without a Horizon backend: no controller and no sound on the Switch. Stop instead.
python3 - "$repo" "$bh" <<'EOF'
import os, re, sys
repo, bh = sys.argv[1:3]
def family(version):  # the versions one semver-compatible requirement can match
    major, minor, patch = (int(x) for x in re.match(r"(\d+)\.(\d+)\.(\d+)", version).groups())
    return (major,) if major else (0, minor) if minor else (0, 0, patch)
lock = open(os.path.join(repo, "Cargo.lock")).read().split("[[package]]")[1:]
locked = [(re.search(r'name = "([^"]+)"', p)[1], re.search(r'version = "([^"]+)"', p)[1]) for p in lock]
table = open(os.path.join(repo, "switch/patches.toml")).read()
errors = []
for key, path, rest in re.findall(r'^([\w-]+) = \{ path = "([^"]+)"(.*)$', table, re.M):
    name = (re.search(r'package = "([^"]+)"', rest) or [None, key])[1]
    where = os.path.join(bh, path.split("/", 2)[2]) if path.startswith("../bevy-horizon/") else os.path.join(repo, path)
    ours = re.search(r'^version = "([^"]+)"', open(os.path.join(where, "Cargo.toml")).read(), re.M)[1]
    same = [v for n, v in locked if n == name and family(v) == family(ours)]
    if not same:
        print(f"note: {name} {ours} ({path}) isn't in the game's crate graph any more; drop its patch", file=sys.stderr)
    errors += [f"{name}: Cargo.lock has {v}, the patch is {ours} ({path}). Re-make the patch from {v}, "
               f"or cargo update -p {name}@{v} --precise {ours}" for v in same if v != ours]
for error in errors:
    print(f"error: {error}", file=sys.stderr)
sys.exit(1 if errors else 0)
EOF

# log.txt's "[DX RUST] build" line names the commit the NRO is from, "-dirty" when other changes were in the tree.
build="$(git -C "$repo" rev-parse --short HEAD)"
git -C "$repo" diff --quiet HEAD -- . ':(exclude)Cargo.lock' || build="$build-dirty"

# The HOME menu's icon (upstream's DX RUST logo on black: switch/icon.jpg) and version (the upstream release the port
# is on, from its tag). ICON= and VERSION= override them.
release="$(git -C "$repo" describe --tags --abbrev=0 2>/dev/null)"
release="${release#v}"

# Symbols stay in the ELF (crash reports and CPU profiles are looked up in it); the NRO holds only code and data.
BEVY=0.19 \
AUTHOR="${AUTHOR:-DX RUST contributors}" \
ICON="${ICON:-$repo/switch/icon.jpg}" \
VERSION="${VERSION:-${release:-0.1.0}-switch}" \
CARGO_ARGS="--config switch/patches.toml --config profile.release.strip=false --config profile.release.debug=1 --config env.DXVIEW_BUILD=\"$build\" ${CARGO_ARGS:-}" \
    "$bh/scripts/build_game.sh" "$repo" dxview dxview "DX RUST"
