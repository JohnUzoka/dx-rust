//! Enhancements: Ambient Occlusion and Soft Shadows (settings AmbientOcclusion / SoftShadows, ours, off by default).
//!
//! - Ambient Occlusion: Bevy's screen-space ambient occlusion on the world camera (depth and normal prepasses);
//!   bsp_shadow.wgsl darkens the BSP's lightmap by it (Bevy only applies it to ambient light, and the world's light
//!   is all lightmap), so corners, grates and the ground under crates and furniture darken. It needs MSAA off on
//!   every camera drawing into the window; SMAA on the last one smooths the edges instead.
//! - Soft Shadows: the real-time shadow lights (shadowlight.rs) get
//!   a light size, so their shadows are filtered with percentage-closer soft shadows: sharp where the caster meets
//!   the ground, blurring as the shadow stretches away (contact hardening).

use bevy::anti_alias::smaa::Smaa;
use bevy::camera::{CameraOutputMode, MsaaWriteback};
use bevy::light::VolumetricFog;
use bevy::pbr::ScreenSpaceAmbientOcclusion;
use bevy::prelude::*;
use bevy::render::render_resource::BlendState;
use bevy::render::view::Msaa;

pub fn ao_on() -> bool {
    crate::settings::get("AmbientOcclusion") > 0.5
}

pub fn soft_on() -> bool {
    crate::settings::get("SoftShadows") > 0.5
}

/// Size of the lights for soft shadows (metres): a lamp's bulb and shade, more or less.
const POINT_SIZE: f32 = 0.12;

#[allow(clippy::type_complexity)]
pub fn postfx_system(
    mut commands: Commands,
    world: Query<(Entity, Has<ScreenSpaceAmbientOcclusion>), (With<crate::FlyCam>, Without<crate::SkyCam>)>,
    mut cams: Query<
        (Entity, &Msaa, Has<Smaa>, Has<crate::weapons::ViewModelCam>, &mut Camera, Has<VolumetricFog>),
        Or<(With<crate::FlyCam>, With<crate::SkyCam>, With<crate::weapons::ViewModelCam>, With<crate::augdisplay::HeatCam>)>,
    >,
    mut points: Query<(&mut SpotLight, &crate::shadowlight::ShadowLight), Without<crate::dynlight::FlashCone>>,
    mut spots: Query<&mut SpotLight, (With<crate::dynlight::FlashCone>, Without<crate::shadowlight::ShadowLight>)>,
    mut bsp: ResMut<Assets<crate::shadowlight::BspMaterial>>,
    // The cameras that draw the world into an image: mirrors' reflections, security feeds, the outfit and aug views.
    others: Query<
        (Entity, &Msaa),
        Or<(
            With<crate::mirror::MirrorCam>,
            With<crate::computers::FeedCam>,
            With<crate::outfit_view::OutfitCam>,
            With<crate::augdisplay::AugViewCam>,
        )>,
    >,
    mut ao_gen: Local<Option<(bool, u32)>>,
) {
    // The cameras' make-up never changes in play: the SSAO pass, the depth / normal prepasses, MSAA off with SMAA.
    // Each of those is part of every material's pipeline, so adding or removing one recompiled them all (a long
    // stall on the Classic / Modern switch) and, removed in the wrong order, crashed Bevy's SSAO (a camera with
    // SSAO but no depth prepass for a frame). The Ambient Occlusion setting is now just how strongly the world's
    // shader uses it.
    let ao = ao_on();
    // The BSP shader's shadow strength goes with it: 0 (Lean Rendering with no shadow lights) skips its walk over
    // the lights in each fragment's cluster.
    let strength = crate::shadowlight::shadow_strength();
    if *ao_gen != Some((ao, strength.to_bits())) {
        *ao_gen = Some((ao, strength.to_bits()));
        let ids: Vec<_> = bsp.ids().collect();
        for id in ids {
            if let Some(mut m) = bsp.get_mut(id) {
                m.extension.params.q.x = if ao { 0.85 } else { 0.0 };
                m.extension.params.p.x = strength;
            }
        }
    }
    // The SSAO pass costs GPU time, so it's only there when Ambient Occlusion is on in either look (now, or in
    // the Modern mix Classic / Modern remembers); once attached it stays for the session.
    static ATTACHED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let wanted = ao || crate::settings::get("Modern.AmbientOcclusion") > 0.5;
    if wanted {
        ATTACHED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let ssao = ATTACHED.load(std::sync::atomic::Ordering::Relaxed) && std::env::var_os("DXVIEW_AO_NOSSAO").is_none();
    for (e, has) in &world {
        if ssao && !has {
            commands.entity(e).insert(ScreenSpaceAmbientOcclusion {
                quality_level: bevy::pbr::ScreenSpaceAmbientOcclusionQualityLevel::Medium,
                constant_object_thickness: 0.3,
            });
        } else if !ssao && has {
            commands.entity(e).remove::<ScreenSpaceAmbientOcclusion>();
        }
    }
    // Anti-Aliasing off (perf.rs) leaves the edges alone; SSAO's SMAA still comes with Ambient Occlusion.
    let want_msaa = if ssao || !crate::perf::msaa() { Msaa::Off } else { Msaa::Sample4 };
    // Lean Rendering: the cameras drawing into the window share one picture, so only the last active one copies it
    // to the window (replacing what is there: the earlier copies were covered by it anyway). Nothing writes the
    // resolved picture between cameras except volumetric fog, so without it the multisampled one already holds
    // everything and the MSAA writebacks are left out.
    let lean = crate::perf::lean_rendering();
    let last = cams.iter().filter(|c| c.4.is_active).max_by_key(|c| c.4.order).map(|c| c.0);
    let fog = cams.iter().any(|c| c.5);
    for (e, msaa, smaa, vm, mut cam, _) in &mut cams {
        if *msaa != want_msaa {
            commands.entity(e).insert(want_msaa);
        }
        if vm && ssao && !smaa {
            commands.entity(e).insert(Smaa::default());
        } else if smaa && !ssao {
            commands.entity(e).remove::<Smaa>();
        }
        let (output, writeback) = if !lean {
            (CameraOutputMode::default(), MsaaWriteback::Auto)
        } else if Some(e) == last {
            let replace = CameraOutputMode::Write { blend_state: Some(BlendState::REPLACE), clear_color: ClearColorConfig::None };
            (replace, if fog { MsaaWriteback::Auto } else { MsaaWriteback::Off })
        } else {
            (CameraOutputMode::Skip, if fog { MsaaWriteback::Auto } else { MsaaWriteback::Off })
        };
        if !same_output(&cam.output_mode, &output) {
            cam.output_mode = output;
        }
        if cam.msaa_writeback != writeback {
            cam.msaa_writeback = writeback;
        }
    }
    // Anti-Aliasing covers the world drawn into images too (4x, Bevy's default, while it's on): with it off, a mirror's
    // reflection was still drawn multisampled, plus a writeback of its sky into the samples (3.9 ms on the Switch).
    let image_msaa = if crate::perf::msaa() { Msaa::Sample4 } else { Msaa::Off };
    for (e, msaa) in &others {
        if *msaa != image_msaa {
            commands.entity(e).insert(image_msaa);
        }
    }
    let soft = soft_on() && crate::shadowlight::level() > 0;
    // Soft shadows cost many shadow-map taps per pixel per light: the two strongest shadow lights get them.
    for (mut p, sl) in &mut points {
        let soft = soft && sl.slot() < 2;
        if p.soft_shadows_enabled != soft {
            p.soft_shadows_enabled = soft;
            p.radius = if soft { POINT_SIZE } else { 0.0 };
        }
    }
    // The flashlight's cone shines from next to the eye, where its shadows are thin edges: PCSS there only added
    // speckled noise on grazing walls. Its shadows stay hard (and the light-shaft spots only light the air).
    for mut s in &mut spots {
        let want = false;
        if s.soft_shadows_enabled != want {
            s.soft_shadows_enabled = want;
            s.radius = 0.0;
        }
    }
}

/// Whether two output modes do the same (`CameraOutputMode` has no `PartialEq`; only these clear colours are set).
fn same_output(a: &CameraOutputMode, b: &CameraOutputMode) -> bool {
    match (a, b) {
        (CameraOutputMode::Skip, CameraOutputMode::Skip) => true,
        (
            CameraOutputMode::Write { blend_state: x, clear_color: cx },
            CameraOutputMode::Write { blend_state: y, clear_color: cy },
        ) => x == y && std::mem::discriminant(cx) == std::mem::discriminant(cy),
        _ => false,
    }
}
