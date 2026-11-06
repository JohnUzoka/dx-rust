DX RUST for Nintendo Switch 0.1.0
=================================

DX RUST (https://github.com/Grissyelda/dx-rust) is Deus Ex rebuilt from the ground up on a modern engine (Rust and
Bevy), running on your own copy of the game. This is its port to a modded Nintendo Switch as homebrew, built with
bevy-horizon (https://github.com/JohnUzoka/bevy-horizon), on DX RUST v0.11.0. It needs your own Deus Ex: Game of the
Year Edition for PC (version 1.112fm); no game data is included.

Install
-------
1. Extract the archive to the root of the SD card: it puts dxview.nro, launcher.txt and this file in
   sdmc:/switch/dxview/.
2. Copy your Deus Ex folder (the one holding System, Maps, Textures, Music and Sounds) to sdmc:/switch/dxview/DeusEx/.
   launcher.txt points the game there; change its GamePath= line if you put it elsewhere.
3. Start it from the Homebrew Menu in title-override mode (hold R while starting a game) or from a forwarder. It
   needs a game's memory: started from the album it has too little.

Controls
--------
The face buttons go by their letters, as on the Switch: A confirms and B goes back in the menus.
  A jump            B crouch          X use / reload (hold: put away)    Y next weapon
  ZR fire           ZL aim            L / R lean                         Left stick click: walk
  Right stick click: third person (hold: change ammo)
  D-pad up: augmentation wheel (hold), down: flashlight, left / right: belt
  Minus: inventory (hold: goals)    Plus: menu
Settings > Enhancements > Controller has the layout picture, the look sensitivity, the dead zone and Aiming
Sensitivity (the stick's speed through a scope: every choice but Unchanged slows it with the zoom, for sniping);
Settings > Controls > Controller Bindings changes what each button does.

Notes
-----
- The first launch compiles the game's shaders as it meets them, so the first minutes stutter; later launches load
  them from the SD card.
- The Switch starts with lean settings (Settings > Enhancements > Performance) and the UI at 150% (Settings >
  Display > UI Scale: 100%, 125% or 150%). HUD Scale Large is the biggest HUD that keeps the belt on screen. The FPS
  Counter is on the Performance list.
- log.txt beside the NRO records each run; include it with bug reports.
- options.txt beside the NRO is optional, for test switches only.

Updates
-------
A few seconds after it starts, the game asks GitHub for a newer release (through the console's internet connection)
and offers it over the menu. Say yes and it downloads while you play, counted at the bottom of the screen; the next
launch runs the new version. Only dxview.nro changes, after its size and checksum are checked; your Deus Ex folder,
settings and saves stay as they are. A no_update_check line in options.txt turns the check off.

Known limits
------------
- Played on a Switch so far: the title, the intro and Liberty Island. Other maps should work but are untested there.
- Handheld, VSync on: the title holds 60 fps; Liberty Island runs at about 42-48 fps.

Source
------
This build is DX RUST's port branch, tag switch-v0.1.0 of https://github.com/JohnUzoka/dx-rust (the port's code in
crates/dxview/src/horizon.rs and switch/: build script, patches), with bevy-horizon at commit b5dfc41
(https://github.com/JohnUzoka/bevy-horizon, branch splash), whose patches/mesa-switch/ and nvk/BUILD_INFO name the
Mesa it carries. DX RUST is free software under the GNU GPL, version 3 or later (the LICENSE file in its repository).

Credits: DX RUST by Grissyelda and contributors; the Switch port and bevy-horizon by John Uzoka. Deus Ex is (c)
Eidos Interactive / Ion Storm; no game data is included.
