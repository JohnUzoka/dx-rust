//! Nintendo Switch (Horizon OS) glue: bevy-horizon's platform layer under the game, and nothing else. On every
//! other platform both entry points do nothing. switch/build.sh has the build and the SD card layout.

use bevy::prelude::*;

/// First thing in `main`: the Switch's log (stdout and stderr go to log.txt beside the NRO, a panic to panic.txt)
/// and the environment the rest of the start-up reads.
pub fn init() {
    #[cfg(target_os = "horizon")]
    {
        // The folder bevy_horizon falls back to; normally the NRO's own name has picked it before main.
        bevy_horizon::init_switch_logging("dxview");
        // SAFETY: no other thread reads the environment yet.
        unsafe {
            // Render-target compression, recommended for 3D (bevy-horizon docs/rendering.md).
            std::env::set_var("BH_RT_COMPRESSION", "1");
            // No HID access and no DualSense on the Switch: don't start the threads that look for one.
            std::env::set_var("DXVIEW_NO_HID", "1");
        }
    }
}

/// Added after `DefaultPlugins`: the Switch's screen, controller, HOME and sleep, dock state and frame loop. The
/// game reads the controller through gilrs, whose Horizon backend switch/patches.toml patches in.
pub struct SwitchPlugin;

impl Plugin for SwitchPlugin {
    fn build(&self, _app: &mut App) {
        #[cfg(target_os = "horizon")]
        {
            _app.add_plugins(bevy_horizon::HorizonPlugin);
            _app.add_systems(Startup, screen_size);
        }
    }
}

/// The window takes the screen's size. bevy_horizon resizes it only when the dock state changes, so a start in
/// handheld mode would keep the saved desktop resolution (1600x900 by default). Docked, its first frame sets
/// 1920x1080 after this.
#[cfg(target_os = "horizon")]
fn screen_size(mut windows: Query<&mut Window, With<bevy::window::PrimaryWindow>>) {
    for mut window in &mut windows {
        window.resolution.set_physical_resolution(1280, 720);
    }
}
