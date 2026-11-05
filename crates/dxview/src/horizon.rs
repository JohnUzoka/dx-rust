//! Nintendo Switch (Horizon OS) glue: bevy-horizon's platform layer under the game, and nothing else. On every
//! other platform both entry points do nothing unless `DXVIEW_AS_SWITCH=1` asks for the Switch's drawing (below).
//! switch/build.sh has the build and the SD card layout.
//!
//! `options.txt` beside the NRO is optional: one option per line, `#` starts a comment. log.txt echoes what was
//! read (`[DX RUST] options.txt: …`); switch/options.txt is a template.
//! - `env NAME=VALUE`: an environment variable, set before the game reads any. `DXVIEW_PROF=1` prints the game's
//!   per-system, per-pass and scene report to log.txt every 5 s (prof.rs); `BEVY_HORIZON_CPU_PROFILE=1` writes
//!   bevy-horizon's cpu-profile.txt; `DXVIEW_NO_PREFETCH=1`, `BH_THREAD_CORES=any`, `BH_ROBUST_BUFFER_ACCESS=0`,
//!   `DXVIEW_ENCODER_PER_PASS=1` and `DXVIEW_ENCODE_ON_RENDER_THREAD=1` (`camera_schedules_in_order`) and
//!   `DXVIEW_NO_REAPER=1` (`submission_reaper`) are A/B switches.
//! - `multi_threaded`: Bevy's multi-threaded executors. Without it, the main world's schedules and the render
//!   world's extract run on one thread, as Skate 3 Rust Engine runs them on the Switch (its findings 63 §6.2, 52.5
//!   to 57.5 fps; and 66 §3): most of DX RUST's ~160 systems take the same resources, so they ran one at a time
//!   anyway, paying for the task pool.
//! - `no_cpu_boost`: keep the normal clocks while a map loads. Without it the CPU runs at 1785 MHz (and the GPU at
//!   76.8 MHz) from launch until the first frame, and again while the loader works (`boost` below).
//! - `gpu_clocks=fast` or `gpu_clocks=max`: the faster handheld GPU configuration an application may ask for
//!   (460.8 MHz instead of 307.2; `max` also takes memory from 1331.2 to 1600 MHz). A measurement: what speeds up
//!   is shading, what doesn't is memory traffic or the CPU.
//! - `memory_log`: keep bevy_horizon's `[Memory]` heartbeat line. Its heap walk made a 30 ms frame in the first
//!   run; without this option it is off.
//! - Missing geometry on the console only (the intro's red wall panels in runs 2-4, run 3's Liberty Island dock until
//!   a reload; a desktop build with the same settings draws both), one switch per run:
//!   - `no_zcull`: ZCULL (the GPU's coarse depth test, which rejects hidden pixels in blocks) off in every pass;
//!   - `draw_empty`, with `gpu_driven`: NVK sets up every indirect draw instead of skipping the ones it reads as empty.
//!   `[Draw check]` lines, 6 s and 20 s after each map arrives, list the level surfaces whose material, texture or
//!   mesh the renderer still hasn't got; none listed with a surface missing on screen points at the GPU side.
//! - `gpu_driven`: upstream's GPU-driven drawing, otherwise off on the Switch: bindless materials (binding arrays),
//!   multi-draw indirect calls, and the level's surfaces culled on the GPU. NVK has no indirect-count draws before
//!   Turing, and run 9 measured both halves on the console: the binding arrays cost the title about 30 ms of GPU time
//!   a frame, and indirect draws without them took up to 80 ms of render-thread encoding on Liberty Island. Without
//!   it every material and lightmap binds on its own (`render_plugin`), every camera draws directly
//!   (`NoIndirectDrawing`), and the level's surfaces keep CPU culling (`DXVIEW_CPU_CULL`, gpudrive.rs).
//! - `ssao`: SSAO as the game decides it (postfx.rs: on when Ambient Occlusion is on in either look, the Modern mix
//!   included, and then for the session). Otherwise it stays off on the Switch (`DXVIEW_AO_NOSSAO`): with the depth
//!   and normal prepass and the SMAA it brings, it cost about 22 ms of GPU time a frame with Ambient Occlusion off
//!   (runs 6b to 9), and the Switch keeps it off until the game holds 60 fps without it (the user's call, 2026-11-02).
//!
//! On the desktop, `DXVIEW_AS_SWITCH=1` draws and schedules as the Switch does without `gpu_driven` and
//! `multi_threaded` (no binding arrays, direct draws, one thread for the main world), so desktop runs measure the
//! Switch's path; run it with `DXVIEW_CPU_CULL=1` and `DXVIEW_AO_NOSSAO=1` as `init` sets them on the console.
//!
//! The settings the Switch starts with (`switch_settings`), unless settings.txt already has a value: Anti-Aliasing and
//! HDR Rendering off, Lean Rendering and Lean Updates on (perf.rs), Open Sea on (water.rs), and UI Scale at 125% for
//! the handheld's small screen (hud.rs). They are on the Settings > Enhancements and Display lists like the rest.
//! While neither Realtime Shadows nor Soft Shadows is on, every 3D camera
//! uses Bevy's one-tap `Hardware2x2` shadow filter instead of its 8- to 9-tap Gaussian (`shadow_filter`): the filter is
//! compiled into every lit shader whether or not a light casts: 450 of the PBR shaders' 3,120 instructions (NAK).

use bevy::prelude::*;

mod gpu_frame_time;

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

/// Bevy's render plugin: without the binding-array features when drawing as the Switch does (`direct_drawing`), so
/// every material and lightmap binds on its own.
pub fn render_plugin() -> bevy::render::RenderPlugin {
    if direct_drawing() {
        use bevy::render::settings::{WgpuFeatures, WgpuSettings};
        return bevy::render::RenderPlugin {
            render_creation: WgpuSettings {
                disabled_features: Some(WgpuFeatures::TEXTURE_BINDING_ARRAY | WgpuFeatures::BUFFER_BINDING_ARRAY),
                ..default()
            }
            .into(),
            ..default()
        };
    }
    bevy::render::RenderPlugin::default()
}

/// The Switch's drawing (module notes): on the console unless options.txt has `gpu_driven`, on the desktop with
/// `DXVIEW_AS_SWITCH=1`.
fn direct_drawing() -> bool {
    #[cfg(target_os = "horizon")]
    return !option("gpu_driven");
    #[cfg(not(target_os = "horizon"))]
    return env_on!("DXVIEW_AS_SWITCH");
}

/// First thing in `main`: the Switch's log (stdout and stderr go to log.txt beside the NRO, a panic to panic.txt),
/// the working folder, options.txt, and the environment the rest of the start-up reads.
pub fn init() {
    #[cfg(target_os = "horizon")]
    {
        // The folder bevy_horizon falls back to; normally the NRO's own name has picked it before main.
        bevy_horizon::init_switch_logging("dxview");
        // Its panic hook, kept for the whole run (`keep_switch_panic_hook`).
        let _ = SWITCH_PANIC_HOOK.set(std::panic::take_hook());
        keep_switch_panic_hook();
        // libnx starts in the NRO's folder already; setting it makes launcher.txt, settings.txt and Save/ certain.
        if let Err(error) = std::env::set_current_dir(&*ROOT) {
            eprintln!("[DX RUST] could not make {} the working folder: {error}", *ROOT);
        }
        eprintln!("[DX RUST] build {}", option_env!("DXVIEW_BUILD").unwrap_or("not named (built without switch/build.sh)"));
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
            if !option("gpu_driven") {
                // Direct draws leave the GPU nothing to cull with: the level's surfaces keep CPU culling (gpudrive.rs).
                std::env::set_var("DXVIEW_CPU_CULL", "1");
            }
            if !option("ssao") {
                // No SSAO whatever Ambient Occlusion says (postfx.rs), until the game holds 60 fps without it.
                std::env::set_var("DXVIEW_AO_NOSSAO", "1");
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
        splash();
        if option("gpu_driven") {
            eprintln!("[DX RUST] options.txt gpu_driven: upstream's GPU-driven drawing (binding arrays, indirect draws, the level culled on the GPU)");
        } else {
            eprintln!("[DX RUST] drawing: no binding arrays, direct draws, the level culled on the CPU (options.txt gpu_driven: upstream's GPU-driven drawing)");
        }
        if option("ssao") {
            eprintln!("[DX RUST] options.txt ssao: SSAO as the game decides it (Ambient Occlusion on in either look)");
        } else {
            eprintln!("[DX RUST] SSAO off, whatever Ambient Occlusion says (options.txt ssao: as the game decides it)");
        }
        switch_settings();
        eprintln!("[DX RUST] settings: {}", settings_line());
        clocks::start(value("gpu_clocks"));
        if option("no_cpu_boost") {
            eprintln!("[Boost] options.txt no_cpu_boost: maps load at the normal clocks");
        } else {
            boost::allow();
            boost::set(true, "the first map loads before the game starts");
        }
    }
}

/// bevy_horizon's panic hook (the panic's message and place to log.txt and panic.txt), taken in `init`. The game's
/// own, `install_crash_log` (main.rs, a Startup system), wraps it and formats a backtrace first; on the Switch
/// libgcc's unwinder aborts there (in `uw_init_context_1`), so run 22 ended with nothing about its panic in the log.
/// PostStartup puts bevy_horizon's back over the game's (`SwitchPlugin`). Set in `init`, not lazily: it has to be
/// taken right after bevy_horizon installs it, before the game's Startup systems put theirs in place.
#[cfg(target_os = "horizon")]
static SWITCH_PANIC_HOOK: std::sync::OnceLock<Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send + 'static>> =
    std::sync::OnceLock::new();

#[cfg(target_os = "horizon")]
fn keep_switch_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        if let Some(hook) = SWITCH_PANIC_HOOK.get() {
            hook(info);
        }
    }));
}

/// From the start of `main` to the game's first frame (run 21: 7.8 s of black screen), the DX RUST logo on black with
/// a bar running under it, drawn by bevy_horizon's splash on the window before Bevy has a surface. The first frame
/// replaces it with upstream's own LOADING screen. `no_splash` in options.txt leaves the screen black, as before.
#[cfg(target_os = "horizon")]
fn splash() {
    use bevy::image::{CompressedImageFormats, ImageSampler, ImageType};
    if option("no_splash") {
        return;
    }
    let logo = Image::from_buffer(
        include_bytes!("../../../assets/dxrust_256.png"),
        ImageType::Extension("png"),
        CompressedImageFormats::NONE,
        true,
        ImageSampler::Default,
        bevy::asset::RenderAssetUsages::MAIN_WORLD,
    );
    let Ok(logo) = logo else {
        eprintln!("[DX RUST] splash: the logo didn't decode");
        return;
    };
    let (lw, lh) = (logo.width() as usize, logo.height() as usize);
    let Some(mut rgba) = logo.data.filter(|d| d.len() == lw * lh * 4) else { return };
    // On black: the colour times its alpha, opaque.
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as u32;
        for c in &mut px[..3] {
            *c = (*c as u32 * a / 255) as u8;
        }
        px[3] = 255;
    }
    let mut first_frames = 0;
    let shown = bevy_horizon::splash::show(1280, 720, std::time::Duration::from_millis(33), move |f| {
        let (w, h, stride) = (f.width as usize, f.height as usize, f.stride);
        let (x0, y0) = (w.saturating_sub(lw) / 2, h.saturating_sub(lh) / 2 - 24);
        // The buffer keeps what is drawn into it: the black and the logo go in once (a few times over, for the
        // display's buffers), then only the bar moves.
        if first_frames < 3 {
            first_frames += 1;
            for row in f.pixels.chunks_mut(stride).take(h) {
                for px in row[..w * 4].chunks_exact_mut(4) {
                    px.copy_from_slice(&[0, 0, 0, 255]);
                }
            }
            for y in 0..lh.min(h - y0) {
                let at = (y0 + y) * stride + x0 * 4;
                f.pixels[at..at + lw * 4].copy_from_slice(&rgba[y * lw * 4..(y + 1) * lw * 4]);
            }
        }
        // A short orange glow running along a dim track under the logo, once every 1.4 s.
        let (bw, glow) = (lw as i32, 72i32);
        let head = ((f.elapsed.as_secs_f32() / 1.4).fract() * (bw + glow) as f32) as i32 - glow;
        let by = y0 + lh + 36;
        for y in by..(by + 3).min(h) {
            let row = &mut f.pixels[y * stride..];
            for x in 0..bw {
                let d = x - head;
                let k = if (0..glow).contains(&d) { 1.0 - ((d - glow / 2) as f32).abs() / (glow / 2) as f32 } else { 0.0 };
                let mix = |dim: f32, lit: f32| (dim + (lit - dim) * k) as u8;
                let at = (x0 + x as usize) * 4;
                row[at..at + 4].copy_from_slice(&[mix(36.0, 240.0), mix(36.0, 122.0), mix(36.0, 30.0), 255]);
            }
        }
    });
    if !shown {
        eprintln!("[DX RUST] splash: none (the screen stays black until the first frame)");
    }
}

/// Added after `DefaultPlugins`: the Switch's screen, controller, HOME and sleep, dock state and frame loop. The
/// game reads the controller through gilrs, whose Horizon backend switch/patches.toml patches in.
pub struct SwitchPlugin;

impl Plugin for SwitchPlugin {
    fn build(&self, _app: &mut App) {
        // The whole-frame GPU timer rides the profiler's switch (prof.rs): every platform, so the desktop renders can check it.
        if std::env::var_os("DXVIEW_PROF").is_some() {
            _app.add_plugins(gpu_frame_time::GpuFrameTimePlugin);
        }
        if direct_drawing() {
            // Bevy wants NoIndirectDrawing on a camera from its spawn: every camera the game makes gets it then.
            _app.add_observer(|add: On<Add, Camera>, mut commands: Commands| {
                commands.entity(add.entity).insert(bevy::render::view::NoIndirectDrawing);
            });
        }
        #[cfg(not(target_os = "horizon"))]
        if env_on!("DXVIEW_AS_SWITCH") {
            single_threaded(_app);
        }
        #[cfg(any(target_os = "horizon", feature = "patched_bevy_render"))]
        if cfg!(target_os = "horizon") || env_on!("DXVIEW_AS_SWITCH") {
            camera_schedules_in_order(_app);
        }
        if cfg!(target_os = "horizon") || env_on!("DXVIEW_AS_SWITCH") {
            submission_reaper(_app);
        }
        #[cfg(any(target_os = "horizon", feature = "patched_bevy_pbr"))]
        {
            _app.add_systems(
                PostUpdate,
                props_skip_prepass.after(bevy::transform::TransformSystems::Propagate).after(crate::batches::batch_system),
            );
            // Lean Rendering's weapon on the world camera (viewcam.rs): the patched mesh shader draws it there.
            crate::viewcam::allow();
            _app.add_systems(Last, view_model_tags);
        }
        #[cfg(target_os = "horizon")]
        {
            _app.add_plugins(bevy_horizon::HorizonPlugin);
            _app.add_systems(Startup, screen_size);
            _app.add_systems(Update, shadow_filter);
            _app.add_systems(PreUpdate, dock_clocks);
            _app.add_systems(Last, (cpu_boost, shadow_lights_log, draw_check_collect, log_app_exit));
            // bevy_horizon's panic hook back over the game's (`SWITCH_PANIC_HOOK`).
            _app.add_systems(PostStartup, keep_switch_panic_hook);
            // The thread that ends the NRO, in the `[Exit]` lines (`exit_backtrace`).
            if !bevy_horizon::register_release_hook(exit_backtrace) {
                eprintln!("[DX RUST] no release hook left for the exit's backtrace");
            }
            if let Some(render) = _app.get_sub_app_mut(bevy::render::RenderApp) {
                render.add_systems(bevy::render::Render, draw_check_report.after(bevy::render::RenderSystems::PrepareAssets));
            }
            if option("draw_empty") {
                let set = bevy_horizon::nvk_tune::set(bevy_horizon::nvk_tune::Knob::SkipEmptyDraws, 0);
                eprintln!("[DX RUST] options.txt draw_empty: NVK sets up every indirect draw, empty ones too ({})", if set { "set" } else { "this NVK has no switch for it" });
            }
            if option("multi_threaded") {
                eprintln!("[DX RUST] options.txt multi_threaded: Bevy's multi-threaded executors");
            } else {
                single_threaded(_app);
            }
            if option("no_zcull") {
                // After HorizonPlugin, which sets ZCULL's direction; from the first render pass on.
                let set = bevy_horizon::nvk_tune::set(bevy_horizon::nvk_tune::Knob::Zcull, 0);
                eprintln!("[DX RUST] options.txt no_zcull: ZCULL off in every render pass ({})", if set { "set" } else { "this NVK has no switch for it" });
            }
        }
    }
}

/// Why the app ended, in log.txt: each `AppExit` the main world is asked for in a frame (a game system, such as the
/// menu's Quit, or bevy_horizon's applet check). Bevy's renderer asks after `Last`, in the render world's extract,
/// once the render thread has gone; switch/patches/bevy_render's `pipelined_rendering.rs` logs that.
#[cfg(target_os = "horizon")]
fn log_app_exit(mut exits: MessageReader<AppExit>, frame: Option<Res<bevy::diagnostic::FrameCount>>) {
    for exit in exits.read() {
        eprintln!("[Exit] AppExit::{exit:?} asked for in frame {}", frame.as_ref().map_or(0, |f| f.0));
    }
}

/// A release hook, run in bevy_horizon's `[Exit]` lines on the thread that ended the NRO: that thread's return
/// addresses, newest first, from its frame records, as ELF addresses (`nro+X`: `addr2line -f -C -e dxview.elf X`).
/// `main` returning shows the C runtime's `exit` under `main`; an `exit()` or `abort()` elsewhere shows its caller.
#[cfg(target_os = "horizon")]
fn exit_backtrace() {
    unsafe extern "C" {
        fn _start();
    }
    // libnx's crt0 is the first thing in the NRO, which the ELF has at 0.
    let base = _start as usize;
    let mut fp: usize;
    // SAFETY: reads this function's frame pointer.
    unsafe { core::arch::asm!("mov {}, x29", out(reg) fp) };
    eprint!("[Exit] the thread that ended the app, newest call first:");
    for _ in 0..32 {
        if fp == 0 || fp % 8 != 0 {
            break;
        }
        // SAFETY: a frame record (the caller's frame pointer, then the return address) on this thread's stack:
        // aligned, and each record is above the last by less than 1 MiB, as one stack's frames are.
        let (next, lr) = unsafe { (*(fp as *const usize), *((fp + 8) as *const usize)) };
        if lr == 0 {
            break;
        }
        eprint!(" nro+{:#x}", lr.wrapping_sub(base));
        if next <= fp || next - fp > 1 << 20 {
            break;
        }
        fp = next;
    }
    eprintln!();
}

/// Props out of the depth prepass (`SkipPrepass`, switch/patches/bevy_pbr): each part of a level mesh actor whose
/// bounding sphere's radius is at most `DXVIEW_PROP_PREPASS_LIMIT` metres (1 by default: crates, chairs, lamps; boats,
/// trees and statues stay) is drawn once instead of twice, a draw less each on the render thread. The
/// level's surfaces and the large meshes keep the prepass and so the main pass's early depth test against them.
/// Tagged in the frame a part appears, before its first extraction (an entity already specialized for the prepass
/// would stay in it); `DXVIEW_PROP_PREPASS_LIMIT=0` keeps every prop in the prepass, as through run 13. Lean
/// Rendering's batches (batches.rs) of parts no larger than that are tagged the same way.
#[cfg(any(target_os = "horizon", feature = "patched_bevy_pbr"))]
fn props_skip_prepass(
    mut commands: Commands,
    parts: Query<(Entity, &ChildOf, &Mesh3d, &GlobalTransform), (Added<Mesh3d>, With<MeshMaterial3d<StandardMaterial>>)>,
    actors: Query<(), With<crate::actors::MeshActorRef>>,
    batches: Query<(Entity, &crate::batches::StaticBatch), Added<crate::batches::StaticBatch>>,
    meshes: Res<Assets<Mesh>>,
) {
    use bevy::camera::primitives::MeshAabb;
    static LIMIT: std::sync::LazyLock<f32> = std::sync::LazyLock::new(|| {
        std::env::var("DXVIEW_PROP_PREPASS_LIMIT").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(1.0)
    });
    if *LIMIT <= 0.0 {
        return;
    }
    for (part, parent, mesh, tf) in &parts {
        if !actors.contains(parent.parent()) {
            continue;
        }
        let Some(aabb) = meshes.get(&mesh.0).and_then(|m| m.compute_aabb()) else { continue };
        let radius = Vec3::from(aabb.half_extents).length() * tf.compute_transform().scale.abs().max_element();
        if radius <= *LIMIT {
            commands.entity(part).insert(bevy::pbr::SkipPrepass);
        }
    }
    for (batch, b) in &batches {
        if b.max_radius <= *LIMIT {
            commands.entity(batch).insert(bevy::pbr::SkipPrepass);
        }
    }
}

/// Lean Rendering's weapon on the world camera (viewcam.rs; switch/patches/bevy_pbr's render/mesh.wgsl and
/// material.rs): every mesh on the weapon's layer carries its camera's projection in a `MeshTag`, and stays out of the
/// depth prepass (its depth goes in front in the main pass alone) and out of frustum culling (it lies around the
/// origin, in its camera's space). Tagged in the frame a mesh appears, before its first extraction; all of them again
/// when that camera's field of view changes (weapons.rs `fov_system`: the window's shape). Harmless while the
/// weapon's own camera draws them: the same sums give its picture.
#[cfg(any(target_os = "horizon", feature = "patched_bevy_pbr"))]
fn view_model_tags(
    mut commands: Commands,
    cam: Query<&Projection, With<crate::weapons::ViewModelCam>>,
    new: Query<
        (Entity, &bevy::camera::visibility::RenderLayers),
        (With<Mesh3d>, Or<(Added<Mesh3d>, Changed<bevy::camera::visibility::RenderLayers>)>),
    >,
    all: Query<(Entity, &bevy::camera::visibility::RenderLayers), With<Mesh3d>>,
    mut current: Local<u32>,
) {
    let Some(Projection::Perspective(p)) = cam.iter().next() else { return };
    let tag = bevy::pbr::view_model_tag(p.fov);
    let retag = tag != *current;
    *current = tag;
    let view = bevy::camera::visibility::RenderLayers::layer(crate::weapons::VIEW_LAYER);
    let mut mark = |part: Entity, layers: &bevy::camera::visibility::RenderLayers| {
        if *layers == view {
            commands.entity(part).insert((
                bevy::mesh::MeshTag(tag),
                bevy::pbr::SkipPrepass,
                bevy::camera::visibility::NoFrustumCulling,
            ));
        }
    };
    if retag {
        all.iter().for_each(|(part, layers)| mark(part, layers));
    } else {
        new.iter().for_each(|(part, layers)| mark(part, layers));
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

/// Every 3D camera's shadow filter: Bevy's one-tap `Hardware2x2` while neither Realtime Shadows nor Soft Shadows is on
/// (the module notes), its Gaussian otherwise. With no light casting, the picture is the same. A change re-specializes
/// the lit pipelines, so it happens only when one of those settings changes.
#[cfg(target_os = "horizon")]
fn shadow_filter(mut commands: Commands, cameras: Query<(Entity, Option<&bevy::light::ShadowFilteringMethod>), With<Camera3d>>) {
    use bevy::light::ShadowFilteringMethod;
    let shadows = crate::settings::get("RealtimeShadows") > 0.5 || crate::settings::get("SoftShadows") > 0.5;
    let want = if shadows { ShadowFilteringMethod::Gaussian } else { ShadowFilteringMethod::Hardware2x2 };
    for (e, now) in &cameras {
        if now != Some(&want) {
            commands.entity(e).insert(want);
        }
    }
}

/// The settings the Switch starts with, where settings.txt has none (a value there, written by the menu or by hand,
/// wins): the lean Performance settings (perf.rs), and Open Sea (water.rs), so the sea doesn't end in black.
#[cfg(target_os = "horizon")]
fn switch_settings() {
    const SWITCH: [(&str, f32); 6] = [
        ("AntiAliasing", 0.0),
        ("HDRRendering", 0.0),
        ("LeanRendering", 1.0),
        ("LeanUpdates", 1.0),
        ("OpenSea", 1.0),
        ("UIScale", 125.0),
    ];
    // The working folder is the NRO's, where settings.rs keeps settings.txt when there is no exe path.
    let saved = std::fs::read_to_string("settings.txt").unwrap_or_default();
    for (key, value) in SWITCH {
        let set = saved.lines().any(|line| line.split_once('=').is_some_and(|(k, _)| k.trim() == key));
        if !set {
            crate::settings::set(key, value);
        }
    }
}

/// The CPU boost follows the map loader (`loader::busy`): on while a map loads, off for play. Once the first map is up
/// its ClassDb holds the code packages, so the boot's read of them (`prefetch_classes`) is let go.
#[cfg(target_os = "horizon")]
fn cpu_boost() {
    let busy = crate::loader::busy();
    boost::set(busy, "a map loads");
    if !busy {
        if let Ok(mut prefetch) = CLASS_PREFETCH.try_lock() {
            prefetch.take();
        }
    }
}

/// Run 21's title setup spent 2.1 s in `ClassDb::open` reading the game's code packages (DeusExCharacters.u,
/// DeusExUI.u and the rest, about 85 MB) whole from the SD card: the Switch can't map a file, so what a PC reads page
/// by page as it's used is read at once. From launch a thread reads them while the title map loads and Bevy starts.
/// unreal_pkg shares a file's bytes with every Package opened on it while one holds them, so the setup's own ClassDb
/// then only reads their tables (or waits for the file being read, never reading it twice).
#[cfg(target_os = "horizon")]
static CLASS_PREFETCH: std::sync::Mutex<Option<std::thread::JoinHandle<unreal_pkg::classes::ClassDb>>> =
    std::sync::Mutex::new(None);

/// Starts the code packages' read (`CLASS_PREFETCH`), once the game folder and its mods are known (main.rs).
/// `no_class_prefetch` in options.txt leaves it to the title's setup, to compare.
pub fn prefetch_classes(_root: &std::path::Path) {
    #[cfg(target_os = "horizon")]
    {
        if option("no_class_prefetch") {
            return;
        }
        let dir = _root.join("System");
        let read = move || {
            let t = std::time::Instant::now();
            let db = unreal_pkg::classes::ClassDb::open(&dir);
            eprintln!("[Boot] code packages read in {:.2} s on a thread of their own", t.elapsed().as_secs_f32());
            db
        };
        match std::thread::Builder::new().name("class prefetch".into()).spawn(read) {
            Ok(h) => *CLASS_PREFETCH.lock().unwrap_or_else(|e| e.into_inner()) = Some(h),
            Err(e) => eprintln!("[Boot] class prefetch thread not started: {e}"),
        }
    }
}

/// A level surface (BSP batch) for the draw check: its mesh, material and base texture, size and place.
#[cfg(target_os = "horizon")]
struct Surface {
    mesh: AssetId<Mesh>,
    material: AssetId<crate::shadowlight::BspMaterial>,
    image: Option<AssetId<Image>>,
    /// Triangles, from the main world's copy of the mesh while it has one: upstream v0.11.0's level meshes live on
    /// the GPU alone once uploaded (`Mesh::indices` panics on those), and then `draw_check_report` counts them there.
    tris: usize,
    at: Vec3,
    seen: bool,
}

/// The surfaces gathered by `draw_check_collect` for the render world to check, with when.
#[cfg(target_os = "horizon")]
static DRAW_CHECK: std::sync::Mutex<Option<(f32, Vec<Surface>)>> = std::sync::Mutex::new(None);

/// `[Draw check]` 6 s and 20 s after each map arrives: hands the level's surfaces to `draw_check_report`, which logs
/// those the renderer can't draw yet. Run 3 lost the Liberty Island dock and runs 2-4 the intro's red bars on the
/// console only; this says whether their material, texture or mesh never got ready, or the GPU dropped them.
#[cfg(target_os = "horizon")]
fn draw_check_collect(
    time: Res<Time<Real>>,
    mats: Res<Assets<crate::shadowlight::BspMaterial>>,
    meshes: Res<Assets<Mesh>>,
    surfaces: Query<(
        &Mesh3d,
        &MeshMaterial3d<crate::shadowlight::BspMaterial>,
        &ViewVisibility,
        Option<&bevy::camera::primitives::Aabb>,
        &GlobalTransform,
    )>,
    mut state: Local<(u64, f32, usize)>,
) {
    const AFTER: [f32; 2] = [6.0, 20.0];
    let now = time.elapsed_secs();
    if crate::loader::new_map(&mut state.0) {
        state.1 = now;
        state.2 = 0;
    }
    let Some(&after) = AFTER.get(state.2) else { return };
    if crate::loader::busy() || now - state.1 < after {
        return;
    }
    state.2 += 1;
    let list = surfaces
        .iter()
        .map(|(mesh, mat, seen, aabb, tf)| Surface {
            mesh: mesh.0.id(),
            material: mat.0.id(),
            image: mats.get(&mat.0).and_then(|m| m.base.base_color_texture.as_ref()).map(|h| h.id()),
            tris: meshes.get(&mesh.0).and_then(|m| m.try_indices().ok()).map_or(0, |i| i.len() / 3),
            at: aabb.map_or(tf.translation(), |a| tf.transform_point(Vec3::from(a.center))),
            seen: seen.get(),
        })
        .collect();
    if let Ok(mut c) = DRAW_CHECK.lock() {
        *c = Some((after, list));
    }
}

/// The render world's half of the draw check (after its assets are prepared).
#[cfg(target_os = "horizon")]
fn draw_check_report(
    materials: Res<bevy::render::erased_render_asset::ErasedRenderAssets<bevy::pbr::PreparedMaterial>>,
    images: Res<bevy::render::render_asset::RenderAssets<bevy::render::texture::GpuImage>>,
    allocator: Res<bevy::render::mesh::allocator::MeshAllocator>,
) {
    let Some((after, list)) = DRAW_CHECK.lock().ok().and_then(|mut c| c.take()) else { return };
    let mut missing = Vec::new();
    for s in &list {
        let material = materials.get(s.material).is_some();
        let texture = s.image.is_none_or(|i| images.get(i).is_some());
        let mesh = allocator.mesh_vertex_slice(&s.mesh).is_some();
        if !(material && texture && mesh) {
            let mut why = Vec::new();
            if !material {
                why.push("material not prepared");
            }
            if !texture {
                why.push("texture not on the GPU");
            }
            if !mesh {
                why.push("mesh not allocated");
            }
            let seen = if s.seen { ", in view" } else { "" };
            let tris = allocator.mesh_index_slice(&s.mesh).map_or(s.tris, |i| i.range.len() / 3);
            missing.push(format!("{tris} tris at {:.0} ({}){seen}", s.at, why.join(", ")));
        }
    }
    let detail = if missing.is_empty() { String::new() } else { format!(": {}", missing.join("; ")) };
    eprintln!("[Draw check] {after:.0} s after arriving: {} level surfaces, {} not ready{detail}", list.len(), missing.len());
}

/// The game's markers on its shadow-casting lights, for `shadow_lights_log`.
#[cfg(target_os = "horizon")]
type LightMarkers = (
    Has<crate::shadowlight::ShadowLight>,
    Has<crate::dynlight::FlashCone>,
    Has<crate::shafts::MoonShaft>,
    Has<crate::shafts::ShaftSpot>,
    Has<crate::shafts::ShaftBeam>,
    Has<crate::clouds::LightningLight>,
);

#[cfg(target_os = "horizon")]
fn light_marker((shadow, cone, moon, spot, beam, lightning): (bool, bool, bool, bool, bool, bool)) -> &'static str {
    if shadow {
        "ShadowLight (Real-Time Shadows)"
    } else if cone {
        "FlashCone (Flashlight Cone)"
    } else if moon || spot || beam {
        "shafts.rs (Light Shafts)"
    } else if lightning {
        "LightningLight (clouds.rs)"
    } else {
        "no marker"
    }
}

/// Logs the shadow-casting lights the renderer takes (visible to a view, as Bevy's extract checks) on the first frame
/// and whenever that set changes: each one adds shadow passes to every frame. (Run 2's single clears of the 512² cube
/// and 1024² shadow maps in the title menu were wgpu zero-filling them before their first use, not shadow passes.)
#[cfg(target_os = "horizon")]
fn shadow_lights_log(
    points: Query<(Entity, &PointLight, &ViewVisibility, &GlobalTransform, LightMarkers)>,
    spots: Query<(Entity, &SpotLight, &ViewVisibility, &GlobalTransform, LightMarkers)>,
    suns: Query<(Entity, &DirectionalLight, &ViewVisibility, &GlobalTransform, LightMarkers)>,
    mut shown: Local<Option<Vec<Entity>>>,
) {
    let mut now = Vec::new();
    for (e, l, v, t, m) in &points {
        if l.shadow_maps_enabled && v.get() {
            now.push((e, "point", l.intensity, t.translation(), light_marker(m)));
        }
    }
    for (e, l, v, t, m) in &spots {
        if l.shadow_maps_enabled && v.get() {
            now.push((e, "spot", l.intensity, t.translation(), light_marker(m)));
        }
    }
    for (e, l, v, t, m) in &suns {
        if l.shadow_maps_enabled && v.get() {
            now.push((e, "directional", l.illuminance, t.translation(), light_marker(m)));
        }
    }
    now.sort_by_key(|l| l.0);
    if shown.as_ref().is_some_and(|shown| now.iter().map(|l| l.0).eq(shown.iter().copied())) {
        return;
    }
    let lights: Vec<String> =
        now.iter().map(|(e, kind, strength, at, marker)| format!("{e} {kind} {marker}, strength {strength}, at {at:.1}")).collect();
    eprintln!("[DX RUST] shadow-casting lights now visible: {}", if lights.is_empty() { "none".into() } else { lights.join("; ") });
    *shown = Some(now.iter().map(|l| l.0).collect());
}

/// The settings that decide what the GPU draws, for the log: a run's numbers mean little without them.
#[cfg(target_os = "horizon")]
fn settings_line() -> String {
    const KEYS: [&str; 18] = [
        "Resolution", "VSync", "RealtimeShadows", "SoftShadows", "AmbientOcclusion", "Bloom", "ModernWater",
        "LightShafts", "FogVolumes", "AtmosphericFog", "FlashlightCone", "DiegeticScreens", "HDTextures",
        "AntiAliasing", "HDRRendering", "LeanRendering", "LeanUpdates", "UIScale",
    ];
    let mut line = format!("Modern {}", crate::remaster::is_modern());
    for key in KEYS {
        line.push_str(&format!(", {key} {}", crate::settings::get(key)));
    }
    line
}

/// One thread for the main world and the render world's extract: on the console unless options.txt has
/// `multi_threaded` (top of file), on the desktop with `DXVIEW_AS_SWITCH=1`.
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
    eprintln!("[DX RUST] one thread for the main world and the render world's extract (options.txt multi_threaded: Bevy's executors)");
}

/// The camera schedules (`Core3d`, which also draws the point and spot lights' shadow maps, and `Core2d`) on one
/// thread: their render systems record into one command encoder a frame (switch/patches/bevy_render), one at a time,
/// and a single-threaded executor runs them in the order upstream submits their command buffers in. Upstream's
/// encoder per pass cost about a quarter of the render thread on the title (run 15). `DXVIEW_ENCODER_PER_PASS=1` keeps
/// an encoder per pass and Bevy's executor for them: the A/B switch. Within a frame, an encoder that holds 64 draws or
/// more is finished (encoded for the GPU) on a thread of its own while the render thread records the passes after it
/// (run 20); `DXVIEW_ENCODE_ON_RENDER_THREAD=1` finishes them all on the render thread at submit, as before.
#[cfg(any(target_os = "horizon", feature = "patched_bevy_render"))]
fn camera_schedules_in_order(app: &mut App) {
    use bevy::ecs::schedule::{ScheduleLabel, SingleThreadedExecutor};
    if env_on!("DXVIEW_ENCODER_PER_PASS") {
        eprintln!("[DX RUST] env DXVIEW_ENCODER_PER_PASS: a command encoder for every render pass, as upstream");
        return;
    }
    if let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) {
        for label in [bevy::core_pipeline::Core3d.intern(), bevy::core_pipeline::Core2d.intern()] {
            render_app.edit_schedule(label, |schedule| {
                schedule.set_executor(SingleThreadedExecutor::new());
            });
        }
    }
    if env_on!("DXVIEW_ENCODE_ON_RENDER_THREAD") {
        eprintln!("[DX RUST] render passes: one command encoder a frame, the camera schedules on one thread, all encoded on the render thread (env DXVIEW_ENCODE_ON_RENDER_THREAD)");
    } else {
        eprintln!("[DX RUST] render passes: the camera schedules on one thread into one encoder, a new one after every 64 draws, those encoded on a thread of their own (env DXVIEW_ENCODER_PER_PASS=1: one per pass; DXVIEW_ENCODE_ON_RENDER_THREAD=1: all on the render thread)");
    }
}

/// Finished frames torn down on a thread of their own. wgpu-core tears a frame down in whatever next polls the device,
/// and on the render thread that's the next `Queue::submit`, before it can submit: NVK resets every command buffer in
/// the frame's command pool, and wgpu-core drops the frame's resource trackers (`drop_glue::<EncoderInFlight>` under
/// `Queue::maintain`, 1.4 ms a frame and a fifth of the render thread's work in run 18's profile). Woken after each
/// frame's submit, the reaper polls the device every half millisecond until the GPU has nothing in flight, so each
/// frame comes down soon after the GPU finishes it, while the render thread prepares the next. A poll that finds
/// nothing finished holds the device's fence for a moment; a submit that comes while a teardown is under way waits for
/// it, which costs that frame no more than doing it itself. `DXVIEW_NO_REAPER=1` leaves it to the render thread, as
/// upstream (the A/B switch).
fn submission_reaper(app: &mut App) {
    if env_on!("DXVIEW_NO_REAPER") {
        eprintln!("[DX RUST] env DXVIEW_NO_REAPER: finished frames are torn down in the render thread's next submit, as upstream");
        return;
    }
    if let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) {
        render_app.add_systems(bevy::render::Render, wake_reaper.in_set(bevy::render::RenderSystems::Cleanup));
        eprintln!("[DX RUST] reaper: finished frames are torn down on a thread of their own (env DXVIEW_NO_REAPER=1: in the render thread's submit)");
    }
}

/// The reaper thread and its wake-up line. Dropped with the render world, which ends the thread and waits for it, so
/// its handle on the device is gone before the app releases the GPU.
struct Reaper {
    wake: Option<std::sync::mpsc::SyncSender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Reaper {
    fn drop(&mut self) {
        self.wake = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// In `Cleanup`, after the frame's submit: starts the reaper on the first frame and wakes it on every one.
fn wake_reaper(device: Res<bevy::render::renderer::RenderDevice>, mut reaper: Local<Option<Reaper>>) {
    let reaper = reaper.get_or_insert_with(|| {
        let (wake, woken) = std::sync::mpsc::sync_channel::<()>(1);
        let device = device.wgpu_device().clone();
        let thread = std::thread::Builder::new()
            .name("reaper".into())
            .spawn(move || {
                while woken.recv().is_ok() {
                    loop {
                        std::thread::sleep(std::time::Duration::from_micros(500));
                        match device.poll(wgpu::PollType::Poll) {
                            Ok(status) if !status.is_queue_empty() => {}
                            _ => break,
                        }
                    }
                }
            })
            .map_err(|e| eprintln!("[DX RUST] reaper: no thread ({e}); the render thread's submit tears frames down"))
            .ok();
        Reaper { wake: thread.as_ref().map(|_| wake), thread }
    });
    if let Some(wake) = &reaper.wake {
        let _ = wake.try_send(());
    }
}

/// A `[Clocks]` line whenever the console is docked or undocked.
#[cfg(target_os = "horizon")]
fn dock_clocks(mode: Res<bevy_horizon::HorizonDisplayMode>) {
    if mode.is_changed() && !mode.is_added() {
        clocks::log("display mode changed");
    }
}

/// The CPU boost while maps load (options.txt `no_cpu_boost` turns it off). libnx's `appletSetCpuBoostMode(FastLoad)`
/// takes the CPU from 1020 to 1785 MHz and the GPU down to 76.8 MHz, so it is on only while nothing but a loading
/// screen is drawn. The Minecraft port's loads took about half as long with it (workshop horizon-port
/// OPTIMIZATIONS.md, O8). Only an application (a forwarder or title takeover) may boost.
#[cfg(target_os = "horizon")]
mod boost {
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    use std::time::Instant;

    /// libnx `ApmCpuBoostMode` (switch/services/apm.h).
    const NORMAL: u32 = 0;
    const FAST_LOAD: u32 = 1;

    unsafe extern "C" {
        fn appletGetAppletType() -> i32;
        fn appletSetCpuBoostMode(mode: u32) -> u32;
    }

    static ALLOWED: AtomicBool = AtomicBool::new(false);
    static ON: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

    pub(super) fn allow() {
        // SAFETY: a libnx getter; libnx sets up the applet service before main.
        let kind = unsafe { appletGetAppletType() };
        if kind == 0 {
            ALLOWED.store(true, Relaxed);
        } else {
            eprintln!("[Boost] applet type {kind}, not an application: maps load at the normal clocks");
        }
    }

    /// Turns the boost on or off, logging each change and how long the boost lasted.
    pub(super) fn set(on: bool, why: &str) {
        if !ALLOWED.load(Relaxed) {
            return;
        }
        let Ok(mut since) = ON.lock() else { return };
        if since.is_some() == on {
            return;
        }
        // SAFETY: allowed for an application (checked in `allow`).
        let rc = unsafe { appletSetCpuBoostMode(if on { FAST_LOAD } else { NORMAL }) };
        if rc != 0 {
            // For example 0x4b59 (libnx IncompatSysVer: the call needs firmware 7.0.0) in an emulator.
            ALLOWED.store(false, Relaxed);
            eprintln!("[Boost] appletSetCpuBoostMode failed with 0x{rc:x}: maps load at the normal clocks");
            return;
        }
        if on {
            *since = Some(Instant::now());
            eprintln!("[Boost] CPU boost on ({why}): CPU 1785 MHz, GPU 76.8 MHz");
        } else {
            let secs = since.take().map_or(0.0, |t| t.elapsed().as_secs_f32());
            eprintln!("[Boost] CPU boost off after {secs:.1} s");
        }
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
