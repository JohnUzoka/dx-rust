//! Nintendo Switch (Horizon OS) glue: bevy-horizon's platform layer under the game, and nothing else. On every
//! other platform both entry points do nothing. switch/build.sh has the build and the SD card layout.
//!
//! `options.txt` beside the NRO is optional: one option per line, `#` starts a comment. log.txt echoes what was
//! read (`[DX RUST] options.txt: …`); switch/options.txt is a template.
//! - `env NAME=VALUE`: an environment variable, set before the game reads any. `DXVIEW_PROF=1` prints the game's
//!   per-system, per-pass and scene report to log.txt every 5 s (prof.rs); `BEVY_HORIZON_CPU_PROFILE=1` writes
//!   bevy-horizon's cpu-profile.txt; `DXVIEW_NO_PREFETCH=1`, `BH_THREAD_CORES=any` and `BH_ROBUST_BUFFER_ACCESS=0`
//!   are A/B switches.
//! - `single_threaded`: the main world's schedules and the render world's extract run on one thread, as Skate 3
//!   Rust Engine runs them on the Switch (its findings 63 §6.2, 52.5 to 57.5 fps; and 66 §3).
//! - `gpu_clocks=fast` or `gpu_clocks=max`: the faster handheld GPU configuration an application may ask for
//!   (460.8 MHz instead of 307.2; `max` also takes memory from 1331.2 to 1600 MHz). A measurement: what speeds up
//!   is shading, what doesn't is memory traffic or the CPU.
//! - `memory_log`: keep bevy_horizon's `[Memory]` heartbeat line. Its heap walk made a 30 ms frame in the first
//!   run; without this option it is off.

use bevy::prelude::*;

/// The NRO's folder: bevy_horizon sets BEVY_ASSET_ROOT to it before main.
#[cfg(target_os = "horizon")]
static ROOT: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    std::env::var("BEVY_ASSET_ROOT").unwrap_or_else(|_| "sdmc:/switch/dxview".into())
});

/// options.txt beside the NRO, one option per line, without comments or blank lines (top of file).
#[cfg(target_os = "horizon")]
static OPTIONS: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    let text = std::fs::read_to_string(format!("{}/options.txt", *ROOT)).unwrap_or_default();
    let lines = text.lines().map(|line| line.split('#').next().unwrap_or_default().trim());
    lines.filter(|line| !line.is_empty()).collect::<Vec<_>>().join("\n")
});

/// Whether options.txt has `name` on a line of its own.
#[cfg(target_os = "horizon")]
fn option(name: &str) -> bool {
    OPTIONS.lines().any(|line| line == name)
}

/// The value of options.txt's `name=value` line.
#[cfg(target_os = "horizon")]
fn value(name: &str) -> Option<&'static str> {
    OPTIONS.lines().find_map(|line| line.strip_prefix(name)?.strip_prefix('=')).map(str::trim)
}

/// First thing in `main`: the Switch's log (stdout and stderr go to log.txt beside the NRO, a panic to panic.txt),
/// the working folder, options.txt, and the environment the rest of the start-up reads.
pub fn init() {
    #[cfg(target_os = "horizon")]
    {
        // The folder bevy_horizon falls back to; normally the NRO's own name has picked it before main.
        bevy_horizon::init_switch_logging("dxview");
        // libnx starts in the NRO's folder already; setting it makes launcher.txt, settings.txt and Save/ certain.
        if let Err(error) = std::env::set_current_dir(&*ROOT) {
            eprintln!("[DX RUST] could not make {} the working folder: {error}", *ROOT);
        }
        let options = &*OPTIONS;
        // SAFETY: no other thread reads the environment yet.
        unsafe {
            // Render-target compression, recommended for 3D (bevy-horizon docs/rendering.md).
            std::env::set_var("BH_RT_COMPRESSION", "1");
            // No HID access and no DualSense on the Switch: don't start the threads that look for one.
            std::env::set_var("DXVIEW_NO_HID", "1");
            if !option("memory_log") {
                std::env::set_var("BEVY_HORIZON_MEMORY_EVERY", "0");
            }
            // Last, so an env line can change any of the above.
            for (name, value) in options.lines().filter_map(|line| line.strip_prefix("env ")?.split_once('=')) {
                if !name.trim().is_empty() {
                    std::env::set_var(name.trim(), value.trim());
                }
            }
        }
        let read = if options.is_empty() { "none".to_string() } else { options.replace('\n', "; ") };
        eprintln!("[DX RUST] options.txt: {read}");
        eprintln!("[DX RUST] settings: {}", settings_line());
        clocks::start(value("gpu_clocks"));
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
            _app.add_systems(PreUpdate, dock_clocks);
            if option("single_threaded") {
                single_threaded(_app);
            }
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

/// The settings that decide what the GPU draws, for the log: a run's numbers mean little without them.
#[cfg(target_os = "horizon")]
fn settings_line() -> String {
    const KEYS: [&str; 13] = [
        "Resolution", "VSync", "RealtimeShadows", "SoftShadows", "AmbientOcclusion", "Bloom", "ModernWater",
        "LightShafts", "FogVolumes", "AtmosphericFog", "FlashlightCone", "DiegeticScreens", "HDTextures",
    ];
    let mut line = format!("Modern {}", crate::remaster::is_modern());
    for key in KEYS {
        line.push_str(&format!(", {key} {}", crate::settings::get(key)));
    }
    line
}

/// options.txt `single_threaded` (top of file).
#[cfg(target_os = "horizon")]
fn single_threaded(app: &mut App) {
    use bevy::app::{
        First, FixedFirst, FixedLast, FixedMain, FixedPostUpdate, FixedPreUpdate, FixedUpdate, Last, Main, PostStartup,
        PostUpdate, PreStartup, RunFixedMainLoop, SpawnScene,
    };
    use bevy::ecs::schedule::{ScheduleLabel, SingleThreadedExecutor};
    let schedules = [
        Main.intern(), PreStartup.intern(), Startup.intern(), PostStartup.intern(), First.intern(),
        PreUpdate.intern(), RunFixedMainLoop.intern(), FixedMain.intern(), FixedFirst.intern(),
        FixedPreUpdate.intern(), FixedUpdate.intern(), FixedPostUpdate.intern(), FixedLast.intern(),
        Update.intern(), SpawnScene.intern(), PostUpdate.intern(), Last.intern(),
    ];
    for label in schedules {
        app.edit_schedule(label, |schedule| {
            schedule.set_executor(SingleThreadedExecutor::new());
        });
    }
    // Multi-threaded, extract hands its small systems to the task pool that the render thread keeps busy, and
    // waits for them (Skate's findings 66 §3).
    if let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) {
        render_app.edit_schedule(bevy::render::ExtractSchedule, |schedule| {
            schedule.set_executor(SingleThreadedExecutor::new());
            // A new executor applies deferred commands at its end; extract's go to the render thread instead
            // (bevy_render `apply_extract_commands`), as before.
            schedule.set_apply_final_deferred(false);
        });
    }
    eprintln!("[DX RUST] options.txt single_threaded: the main world and the render world's extract run on one thread");
}

/// A `[Clocks]` line whenever the console is docked or undocked.
#[cfg(target_os = "horizon")]
fn dock_clocks(mode: Res<bevy_horizon::HorizonDisplayMode>) {
    if mode.is_changed() && !mode.is_added() {
        clocks::log("display mode changed");
    }
}

/// `[Clocks]` lines, so a log says which clocks its frames had, and options.txt's `gpu_clocks`. The configuration
/// values are switchbrew's "PTM services" PerformanceConfiguration table.
#[cfg(target_os = "horizon")]
mod clocks {
    /// libnx `ApmPerformanceMode`: Normal is the handheld mode, Boost the docked one.
    const HANDHELD: i32 = 0;
    const DOCKED: i32 = 1;
    /// CPU 1020 / GPU 460.8 / memory 1331.2 MHz, and the same with memory at 1600 MHz.
    const HANDHELD_FAST: u32 = 0x9222_0008;
    const HANDHELD_MAX: u32 = 0x9222_0007;

    unsafe extern "C" {
        fn appletGetAppletType() -> i32;
        fn appletGetOperationMode() -> i32;
        fn apmGetPerformanceConfiguration(mode: i32, configuration: *mut u32) -> u32;
        fn apmSetPerformanceConfiguration(mode: i32, configuration: u32) -> u32;
        fn clkrstInitialize() -> u32;
        fn clkrstExit();
        fn clkrstOpenSession(session: *mut ClkrstSession, module_id: u32, unknown: u32) -> u32;
        fn clkrstGetClockRate(session: *mut ClkrstSession, hz: *mut u32) -> u32;
        fn clkrstCloseSession(session: *mut ClkrstSession);
    }

    /// libnx `ClkrstSession` is one 16-byte `Service`; this is larger and at least as aligned.
    #[repr(C, align(8))]
    struct ClkrstSession([u8; 32]);

    /// Logs the clocks at launch and applies `gpu_clocks`. Only an application has an apm session.
    pub(super) fn start(gpu: Option<&str>) {
        // SAFETY: a libnx getter; libnx sets up the applet service before main.
        if unsafe { appletGetAppletType() } != 0 {
            eprintln!("[Clocks] not running as an application, so the system's clocks stay");
            return;
        }
        log("at launch");
        let handheld = match gpu {
            None | Some("system") => return,
            Some("fast") => HANDHELD_FAST,
            Some("max") => HANDHELD_MAX,
            Some(other) => {
                eprintln!("[Clocks] gpu_clocks={other} isn't fast, max or system; the system's clocks stay");
                return;
            }
        };
        // SAFETY: libnx sets apm up for an application (switch/services/apm.h).
        let rc = unsafe { apmSetPerformanceConfiguration(HANDHELD, handheld) };
        eprintln!("[Clocks] options.txt gpu_clocks: asked for handheld configuration 0x{handheld:08x}, result 0x{rc:x}");
        log("after gpu_clocks");
    }

    /// Docked or handheld, both modes' configurations, and the clocks running now.
    pub(super) fn log(when: &str) {
        let configuration = |mode| {
            let mut value = 0;
            // SAFETY: apm is set up for an application (checked in `start`); `value` is written.
            let rc = unsafe { apmGetPerformanceConfiguration(mode, &mut value) };
            if rc == 0 { format!("0x{value:08x}") } else { format!("? (0x{rc:x})") }
        };
        // SAFETY: a libnx getter of the applet state it keeps.
        let mode = if unsafe { appletGetOperationMode() } == 1 { "docked" } else { "handheld" };
        let running = rates().unwrap_or_else(|| "unknown (clkrst didn't open)".into());
        eprintln!(
            "[Clocks] {when}: {mode}; configurations handheld {}, docked {}; running at {running}",
            configuration(HANDHELD),
            configuration(DOCKED)
        );
    }

    /// The clocks as `CPU 1020.0 MHz, GPU 307.2 MHz, memory 1331.2 MHz`, read through clkrst as Skate 3 Rust
    /// Engine's horizon.rs reads them.
    fn rates() -> Option<String> {
        // SAFETY: libnx's clkrst calls; each session is opened and closed here, the service released below.
        if unsafe { clkrstInitialize() } != 0 {
            return None;
        }
        let mhz = |module| {
            let mut session = ClkrstSession([0; 32]);
            // SAFETY: as above.
            if unsafe { clkrstOpenSession(&mut session, module, 3) } != 0 {
                return "?".to_string();
            }
            let mut hz = 0;
            // SAFETY: as above.
            let rc = unsafe { clkrstGetClockRate(&mut session, &mut hz) };
            unsafe { clkrstCloseSession(&mut session) };
            if rc == 0 { format!("{:.1}", f64::from(hz) / 1e6) } else { "?".to_string() }
        };
        // libnx pcv.h `PcvModuleId_CpuBus`, `_GPU` and `_EMC`.
        let rates = format!("CPU {} MHz, GPU {} MHz, memory {} MHz", mhz(0x4000_0001), mhz(0x4000_0002), mhz(0x4000_0039));
        // SAFETY: as above.
        unsafe { clkrstExit() };
        Some(rates)
    }
}
