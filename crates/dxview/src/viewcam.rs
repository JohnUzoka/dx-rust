//! Lean Rendering: the first-person weapon drawn by the world camera instead of a 3D camera of its own, where the
//! renderer can (the Switch build's bevy_pbr patch: horizon.rs calls `allow`). Every camera costs each frame whatever
//! it draws: its view's light clusters, mesh preprocessing and bind groups, and the work of each of its passes. With
//! the weapon's camera off, the Switch's title took 8.8 ms of render thread instead of 13.5 and 3.4 ms less GPU time
//! (run 16), though that launch also left out the three jobs the world camera takes over here: the brightness pass,
//! the HUD and the picture's copy to the screen (it showed a black screen).
//!
//! The weapon keeps its camera's space: weapons.rs places its parts around the origin as before, and the patched mesh
//! shader projects them as that camera would, in front of the world (their `MeshTag`, set by horizon.rs). The weapon's
//! camera stays, inactive, and draws again whenever the world camera isn't the last camera drawing into the window or
//! the last camera has a job only it does: Upscaling (its camera draws after the world's), the heat view (likewise),
//! Bloom, SSAO's SMAA, or Lean Rendering off. `DXVIEW_VIEW_CAM=1` keeps the weapon's camera (the A/B switch).

use bevy::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};

static ALLOWED: AtomicBool = AtomicBool::new(false);
static ON: AtomicBool = AtomicBool::new(false);

/// The renderer can draw the weapon with the world camera (horizon.rs, with switch/patches/bevy_pbr).
#[allow(dead_code)] // only builds with the patch call it
pub fn allow() {
    ALLOWED.store(true, Ordering::Relaxed);
}

/// This frame the world camera draws the weapon, the brightness pass, the HUD and the copy to the screen.
pub fn world_draws_weapon() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Decides for the frame, before the systems that read it (thirdperson's layers, upscale's output), then turns the
/// weapon's camera on or off and moves the brightness pass and the HUD to the camera that draws last.
#[allow(clippy::type_complexity)]
pub fn view_camera_system(
    mut commands: Commands,
    world: Single<
        (
            Entity,
            Option<&crate::dynlight::Brightness>,
            Has<bevy::ui::IsDefaultUiCamera>,
            Has<bevy::pbr::ScreenSpaceAmbientOcclusion>,
        ),
        (With<crate::FlyCam>, Without<crate::SkyCam>, Without<crate::weapons::ViewModelCam>),
    >,
    vm: Single<
        (Entity, &mut Camera, Option<&crate::dynlight::Brightness>, Has<bevy::ui::IsDefaultUiCamera>),
        (With<crate::weapons::ViewModelCam>, Without<crate::FlyCam>),
    >,
    heat: Query<&Camera, (With<crate::augdisplay::HeatCam>, Without<crate::weapons::ViewModelCam>)>,
    mut logged: Local<Option<bool>>,
) {
    // Without the patch (upstream's builds) the weapon's camera stays as upstream has it.
    if !ALLOWED.load(Ordering::Relaxed) {
        return;
    }
    let (w, w_brightness, w_ui, ssao) = *world;
    let (v, mut vm_cam, v_brightness, v_ui) = vm.into_inner();
    let held_back = [
        (env_on!("DXVIEW_VIEW_CAM"), "env DXVIEW_VIEW_CAM"),
        (!crate::perf::lean_rendering(), "Lean Rendering off"),
        (crate::upscale::mode() != 0, "Upscaling"),
        (crate::glow::on(), "Bloom"),
        (ssao, "SSAO"),
        (heat.iter().any(|c| c.is_active), "the heat view"),
    ]
    .into_iter()
    .find_map(|(held, why)| held.then_some(why));
    let on = held_back.is_none();
    ON.store(on, Ordering::Relaxed);
    if *logged != Some(on) {
        *logged = Some(on);
        match held_back {
            None => eprintln!("[DX RUST] the weapon: drawn by the world camera, its own camera off"),
            Some(why) => eprintln!("[DX RUST] the weapon: drawn by its own camera ({why})"),
        }
    }
    if vm_cam.is_active == on {
        vm_cam.is_active = !on;
    }
    let ((last, last_brightness, last_ui), (other, other_brightness, other_ui)) =
        if on { ((w, w_brightness, w_ui), (v, v_brightness, v_ui)) } else { ((v, v_brightness, v_ui), (w, w_brightness, w_ui)) };
    if let Some(b) = other_brightness {
        if last_brightness.is_none() {
            commands.entity(last).insert(*b);
        }
        commands.entity(other).remove::<crate::dynlight::Brightness>();
    }
    if !last_ui {
        commands.entity(last).insert(bevy::ui::IsDefaultUiCamera);
    }
    if other_ui {
        commands.entity(other).remove::<bevy::ui::IsDefaultUiCamera>();
    }
}
