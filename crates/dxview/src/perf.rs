//! Performance switches (Settings > Enhancements > Performance). Their defaults keep the PC's picture and
//! behaviour; the Switch build starts with the lean values (horizon.rs). Systems read them every frame, so they are
//! cached here and read again only when the settings change (`settings::generation`). The FPS Counter lives here too.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering::Relaxed};

const MSAA: u8 = 1;
const HDR: u8 = 2;
const LEAN_RENDERING: u8 = 4;
const LEAN_UPDATES: u8 = 8;
const FPS_COUNTER: u8 = 16;

/// The settings generation `FLAGS` was read at.
static READ_AT: AtomicU64 = AtomicU64::new(u64::MAX);
static FLAGS: AtomicU8 = AtomicU8::new(0);

fn flags() -> u8 {
    let generation = crate::settings::generation();
    if READ_AT.load(Relaxed) != generation {
        let on = |key: &str, bit: u8| if crate::settings::get(key) > 0.5 { bit } else { 0 };
        let flags = on("AntiAliasing", MSAA) | on("HDRRendering", HDR) | on("LeanRendering", LEAN_RENDERING)
            | on("LeanUpdates", LEAN_UPDATES) | on("FPSCounter", FPS_COUNTER);
        FLAGS.store(flags, Relaxed);
        READ_AT.store(generation, Relaxed);
    }
    FLAGS.load(Relaxed)
}

/// 4x MSAA on the cameras that draw into the window while SSAO is off (setting AntiAliasing). Off: no
/// anti-aliasing at all.
pub fn msaa() -> bool {
    flags() & MSAA != 0
}

/// HDR (16-bit float) render targets (setting HDRRendering). Off: 8-bit targets, unless Bloom wants HDR.
pub fn hdr() -> bool {
    flags() & HDR != 0
}

/// Skip GPU work that adds no pixels (setting LeanRendering): duplicate copies to the window, MSAA writebacks,
/// shadow lookups with no shadow lights, Modern Water's fine mesh in Classic.
pub fn lean_rendering() -> bool {
    flags() & LEAN_RENDERING != 0
}

/// Skip CPU work that changes nothing (setting LeanUpdates): redrawing unchanged menu, HUD and conversation windows,
/// rewriting hidden text and unchanged components, posing what nobody sees.
pub fn lean_updates() -> bool {
    flags() & LEAN_UPDATES != 0
}

/// The FPS Counter in the top-right corner (setting FPSCounter; fps.rs).
pub fn fps_counter() -> bool {
    flags() & FPS_COUNTER != 0
}
