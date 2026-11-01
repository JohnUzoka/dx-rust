//! ElectricityEmitter: a crackling beam shot out along the emitter's rotation (jittered by up to randomAngle each
//! frame) that stops at the level or a door and shocks the first thing in it for DamageAmount every damageTime.
//! It flickers: every flickerTime there's an even chance the beam is hidden (and harmless) for that slice.

use crate::actors::{Blockers, World};
use crate::ai::PlayerHealth;
use crate::movers::Movers;
use crate::weapons::{image_for, RuntimeAssets};
use crate::{LevelRes, Player};
use bevy::{
    asset::RenderAssetUsages, mesh::Indices, prelude::*, render::render_resource::PrimitiveTopology,
};
use bevy::camera::{primitives::Aabb, visibility::{NoAutoAabb, NoFrustumCulling}};
use dx_level::collision::{trace_cylinder, Cylinder};
use dx_level::ue_to_bevy;
use glam::DVec3;
use imsim_core::movement::CollisionWorld;
use unreal_pkg::props::{find, Value};

#[derive(Component)]
pub struct Emitter {
    pub tag: String,
    pos: DVec3,
    rot: [i32; 3],
    random_angle: f64,
    damage: f32,
    damage_time: f32,
    flicker: bool,
    flicker_time: f32,
    on: bool,
    hidden: bool,
    last_damage: f32,
    last_flicker: f32,
    /// Lean Updates: the bolt mesh's normals, UVs and indices are the ones `emitter_system` writes (not
    /// beam_mesh's), so a bolt with as many segments only needs its positions rewritten.
    canonical: bool,
}

fn beam_mesh() -> Mesh {
    // Two crossed unit quads along +X (0..1), 1 wide.
    let pos = vec![
        [0.0, -0.5, 0.0],
        [1.0, -0.5, 0.0],
        [1.0, 0.5, 0.0],
        [0.0, 0.5, 0.0],
        [0.0, 0.0, -0.5],
        [1.0, 0.0, -0.5],
        [1.0, 0.0, 0.5],
        [0.0, 0.0, 0.5],
    ];
    let uv = vec![
        [0.0, 0.0],
        [1.0, 0.0],
        [1.0, 1.0],
        [0.0, 1.0],
        [0.0, 0.0],
        [1.0, 0.0],
        [1.0, 1.0],
        [0.0, 1.0],
    ];
    let nrm = vec![[0.0, 0.0, 1.0]; 8];
    let mut m = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    m.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
    m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, nrm);
    m.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
    m.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7]));
    m
}

pub fn spawn_emitters(
    commands: &mut Commands,
    level: &dx_level::Level,
    assets: &mut RuntimeAssets,
    images: &mut Assets<Image>,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) {
    for a in level
        .actors
        .iter()
        .filter(|a| a.class == "ElectricityEmitter")
    {
        let p = &a.props;
        let f = |n: &str, d: f32| match find(p, n) {
            Some(Value::Float(v)) => *v,
            Some(Value::Int(v)) => *v as f32,
            _ => d,
        };
        let b = |n: &str, d: bool| match find(p, n) {
            Some(Value::Bool(v)) => *v,
            _ => d,
        };
        let tex = match find(p, "beamTexture") {
            Some(Value::Path(t)) => t.clone(),
            _ => "Effects.Electricity.Nano_SFX".into(),
        };
        // The original draws a thin, jagged bolt; a flat electric blue reads the same at this width.
        let _ = image_for(assets, images, &tex);
        let mat = materials.add(StandardMaterial {
            base_color: Color::LinearRgba(LinearRgba::rgb(0.5, 0.6, 2.5)),
            unlit: true,
            double_sided: true,
            cull_mode: None,
            alpha_mode: AlphaMode::Add,
            ..default()
        });
        let mesh = meshes.add(beam_mesh());
        let l = a.location();
        let rot = match find(p, "Rotation") {
            Some(Value::Rotator(r)) => *r,
            _ => [0; 3],
        };
        commands.spawn((
            Mesh3d(mesh),
            bevy::camera::visibility::NoFrustumCulling,
            MeshMaterial3d(mat),
            Transform::default(),
            Visibility::Hidden,
            Emitter {
                tag: crate::devices::name_prop(p, "Tag"),
                pos: DVec3::new(l[0] as f64, l[1] as f64, l[2] as f64),
                rot,
                random_angle: f("randomAngle", 8192.0) as f64,
                damage: f("DamageAmount", 2.0),
                damage_time: f("damageTime", 0.2),
                flicker: b("bFlicker", true),
                flicker_time: f("flickerTime", 0.02),
                on: b("bInitiallyOn", true),
                hidden: false,
                last_damage: 0.0,
                last_flicker: 0.0,
                canonical: false,
            },
        ));
    }
}

#[allow(clippy::too_many_arguments)]
pub fn emitter_system(
    mut commands: Commands,
    time: Res<Time>,
    level: Res<LevelRes>,
    blockers: Res<Blockers>,
    mv: Res<Movers>,
    player: Res<Player>,
    mut ph: ResMut<PlayerHealth>,
    mut q: Query<(
        Entity,
        &mut Emitter,
        &mut Transform,
        &mut Visibility,
        &Mesh3d,
        &ViewVisibility,
        Option<&mut Aabb>,
        Has<NoFrustumCulling>,
    )>,
    mut meshes: ResMut<Assets<Mesh>>,
    (mut pawn_hits, mut deco_hits, mut seed): (
        ResMut<crate::damage::PawnHits>,
        ResMut<crate::destruct::DecoHits>,
        Local<u64>,
    ),
) {
    let _prof = crate::prof::scope("electricity::emitter_system");
    let dt = time.delta_secs();
    if *seed == 0 {
        *seed = 0xE1EC_7A1C;
    }
    let mut rnd = || {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((*seed >> 33) as f64) / (1u64 << 31) as f64
    };
    let lvl = &level.0;
    let pc = Cylinder {
        center: player.body.location,
        radius: player.params.radius,
        height: player.params.height,
        actor: usize::MAX,
    };
    let lean = crate::perf::lean_updates();
    for (ent, mut e, mut tf, mut vis, mesh3d, seen, aabb, no_cull) in &mut q {
        // Shown last frame (only this system sets it).
        let was_shown = *vis == Visibility::Inherited;
        if !lean && !no_cull {
            // Lean Updates was switched off: back to drawing the bolt unculled.
            commands.entity(ent).insert(NoFrustumCulling);
        }
        if !e.on {
            if !lean || *vis != Visibility::Hidden {
                *vis = Visibility::Hidden;
            }
            continue;
        }
        if e.flicker {
            e.last_flicker += dt;
            if e.last_flicker >= e.flicker_time {
                e.last_flicker = 0.0;
                e.hidden = rnd() < 0.5;
            }
        }
        if e.hidden {
            if !lean || *vis != Visibility::Hidden {
                *vis = Visibility::Hidden;
            }
            continue;
        }
        // LaserEmitter.TurnOn: AmbientSound Electricity4 at SoundVolume 128 (0 while the flicker hides the beam),
        // SoundRadius 64. Held only when the listener can see it or is close: through the ground it would carry
        // 1600 units up into the open.
        {
            let ear = player.body.location + DVec3::new(0.0, 0.0, player.eye_height);
            let d = ear.distance(e.pos);
            // Silent behind the logo on the title map.
            let heard = !crate::menus::intro_map()
                && d < 1625.0
                && (d < 150.0 || lvl.collision.trace(ear, e.pos, DVec3::ZERO).fraction > 0.9);
            if heard {
                let key = 0xE_0000_0000u64 | ((e.pos.x as i64 as u64 & 0xffff) << 16) | (e.pos.y as i64 as u64 & 0xffff);
                crate::sound::hold(key, "Ambient.Ambient.Electricity4", Some(e.pos), 0.7 * 2.0 * 128.0 / 255.0, 1625.0, 1.0);
            }
        }
        // CalcTrace: jitter the direction, trace 5000 units, stop at the level or a mover.
        let ra = e.random_angle;
        let r = [
            e.rot[0] + ((0.5 - rnd()) * ra) as i32,
            e.rot[1] + ((0.5 - rnd()) * ra) as i32,
            0,
        ];
        let unit = std::f64::consts::TAU / 65536.0;
        let (p, y) = (r[0] as f64 * unit, r[1] as f64 * unit);
        let dir = DVec3::new(p.cos() * y.cos(), p.cos() * y.sin(), p.sin());
        let start = e.pos;
        let end = start + dir * 5000.0;
        let world = World {
            level: lvl,
            blockers: &blockers.list,
            ignore: None,
            pushable: None,
            movers: Some(&mv.rt),
        };
        let h = world.trace(start, end, DVec3::ZERO);
        let mut frac = h.fraction;
        let mut hit_player = false;
        if let Some(t) = trace_cylinder(&pc, start, end, DVec3::ZERO) {
            if t.fraction < frac && !ph.dead {
                frac = t.fraction;
                hit_player = true;
            }
        }
        e.last_damage += dt;
        if e.last_damage >= e.damage_time && (hit_player || h.actor.is_some()) {
            e.last_damage = 0.0;
            let at = start + (end - start) * frac;
            if hit_player {
                ph.damage(e.damage, "Shocked", DVec3::ZERO);
            } else if let Some(b) = h.actor {
                let a = &lvl.actors[blockers.list[b].actor];
                if find(&a.props, "GroundSpeed").is_some() {
                    pawn_hits.0.push(crate::damage::PawnHit {
                        blocker: b,
                        damage: e.damage,
                        damage_type: "Shocked",
                        hit_loc: at,
                        attacker: None,
                        attacker_pos: start,
                    });
                } else {
                    deco_hits.0.push(crate::destruct::DecoHit {
                        blocker: b,
                        damage: e.damage,
                        damage_type: "Shocked",
                    });
                }
            }
        }
        // Draw the bolt: a thin zig-zag (a kink every ~48 units, a few units off the line), crossed quads per
        // segment, rebuilt every frame so it crackles.
        let len = 5000.0 * frac;
        let end_pt = start + dir * len;
        let side = dir.cross(DVec3::Z).normalize_or(DVec3::X);
        let up = side.cross(dir).normalize_or(DVec3::Z);
        let n = ((len / 48.0).ceil() as usize).clamp(1, 120);
        if lean {
            // Lean Updates: the bolt is frustum-culled against bounds that hold any jitter of this frame's beam
            // (5 units each way off the line plus the quads' half width, rounded up), so it is only drawn where
            // it can show. One nobody saw last frame keeps its old mesh: its jitter is random anyway, and the
            // generator steps exactly as if it had been built, so the flicker and the beam's aim don't change.
            let (a, b) = (
                Vec3::from(ue_to_bevy(start.as_vec3().to_array())),
                Vec3::from(ue_to_bevy(end_pt.as_vec3().to_array())),
            );
            let r = Vec3::splat(9.0 * dx_level::METERS_PER_UNIT);
            let bounds = Aabb::from_min_max(a.min(b) - r, a.max(b) + r);
            match aabb {
                Some(mut v) if !no_cull => *v = bounds,
                _ => {
                    commands.entity(ent).insert((bounds, NoAutoAabb)).remove::<NoFrustumCulling>();
                }
            }
            if was_shown && !seen.get() {
                for _ in 0..2 * (n - 1) {
                    rnd();
                }
                continue;
            }
        }
        let mut pts = Vec::with_capacity(n + 1);
        for i in 0..=n {
            let t = i as f64 / n as f64;
            let mut p = start + (end_pt - start) * t;
            if i != 0 && i != n {
                p += side * ((rnd() - 0.5) * 10.0) + up * ((rnd() - 0.5) * 10.0);
            }
            pts.push(Vec3::from(ue_to_bevy(p.as_vec3().to_array())));
        }
        let w = 1.5 * dx_level::METERS_PER_UNIT;
        let (sv, uv_) = (
            Vec3::from(ue_to_bevy(side.as_vec3().to_array())) * w,
            Vec3::from(ue_to_bevy(up.as_vec3().to_array())) * w,
        );
        if lean {
            // The same mesh as below, with the positions written over the old ones when the bolt has as many
            // segments as last time (its normals, UVs and indices depend on nothing else).
            let pts = &pts;
            let verts = move || {
                (0..n)
                    .flat_map(move |k| {
                        let (a, b) = (pts[k], pts[k + 1]);
                        [sv, uv_].map(move |off| {
                            [(a - off).to_array(), (b - off).to_array(), (b + off).to_array(), (a + off).to_array()]
                        })
                    })
                    .flatten()
            };
            if let Some(mut m) = meshes.get_mut(&mesh3d.0) {
                let nv = n * 8;
                let in_place = e.canonical
                    && match m.attribute_mut(Mesh::ATTRIBUTE_POSITION) {
                        Some(bevy::mesh::VertexAttributeValues::Float32x3(dst)) if dst.len() == nv => {
                            for (d, p) in dst.iter_mut().zip(verts()) {
                                *d = p;
                            }
                            true
                        }
                        _ => false,
                    };
                if !in_place {
                    let idx: Vec<u32> = (0..2 * n as u32)
                        .flat_map(|q| {
                            let b = q * 4;
                            [b, b + 1, b + 2, b, b + 2, b + 3]
                        })
                        .collect();
                    m.insert_attribute(Mesh::ATTRIBUTE_POSITION, verts().collect::<Vec<[f32; 3]>>());
                    m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; nv]);
                    m.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0f32, 0.0]; nv]);
                    m.insert_indices(Indices::U32(idx));
                    e.canonical = true;
                }
            }
            if *tf != Transform::IDENTITY {
                *tf = Transform::IDENTITY;
            }
            if !was_shown {
                *vis = Visibility::Inherited;
            }
            continue;
        }
        let mut pos: Vec<[f32; 3]> = Vec::new();
        let mut idx: Vec<u32> = Vec::new();
        for k in 0..n {
            let (a, b) = (pts[k], pts[k + 1]);
            for off in [sv, uv_] {
                let base = pos.len() as u32;
                pos.extend_from_slice(&[
                    (a - off).to_array(),
                    (b - off).to_array(),
                    (b + off).to_array(),
                    (a + off).to_array(),
                ]);
                idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            }
        }
        if let Some(mut m) = meshes.get_mut(&mesh3d.0) {
            let nv = pos.len();
            m.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
            m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; nv]);
            m.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0f32, 0.0]; nv]);
            m.insert_indices(Indices::U32(idx));
        }
        *tf = Transform::IDENTITY;
        *vis = Visibility::Inherited;
    }
}

impl Emitter {
    /// Trigger turns it on, UnTrigger off.
    pub fn set_on(&mut self, on: bool) {
        self.on = on;
    }
}
