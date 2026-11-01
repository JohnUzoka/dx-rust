//! The GPU's time for a whole frame, beside the per-pass times of `RenderDiagnosticsPlugin`. On with the profiler
//! (`env DXVIEW_PROF=1`), where the device has timestamp queries inside encoders (NVK and lavapipe do; the game
//! logs a line when it doesn't). Ported from Skate 3 Rust Engine's `gpu_frame_time.rs` (Bevy 0.18, render graph
//! nodes). Bevy 0.19 has no render graph nodes: the camera passes run in the `RenderGraph` schedule between its
//! `Begin` and `Finish` sets, so the first timestamp is written by a `Begin` system and the last by a `Finish` system,
//! each in its own command buffer. The queue runs command buffers in submission order, so they bracket the frame's
//! passes and submits the same way.
//!
//! The pass timers cover only their passes, and on the Switch they summed to less than the frame. Two timestamps
//! around the whole graph tell the rest apart:
//!
//! - `render/dx_frame/busy_gpu`: from the frame's first command to its last. Less the sum of the pass times, it is the
//!   work inside the frame that no pass timer covers.
//! - `render/dx_frame/wait_gpu`: from the previous frame's last command to this frame's first. Idle time, plus what
//!   runs between two frames' graphs: the uploads, the screenshot and readback copies, and any wait for the
//!   compositor to release the swapchain image.
//!
//! Both are read back a frame or two late, into `DiagnosticsStore`, and `prof.rs` prints their averages with the
//! other `render/` timers.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU8, Ordering},
};

use bevy::{
    diagnostic::{Diagnostic, DiagnosticMeasurement, DiagnosticPath, DiagnosticsStore},
    platform::time::Instant,
    prelude::*,
    render::{
        RenderApp, RenderStartup,
        renderer::{RenderDevice, RenderGraph, RenderGraphSystems, RenderQueue},
    },
};

pub(crate) const BUSY: DiagnosticPath = DiagnosticPath::const_new("render/dx_frame/busy_gpu");
pub(crate) const WAIT: DiagnosticPath = DiagnosticPath::const_new("render/dx_frame/wait_gpu");

/// Frames whose timestamps can be on their way back at once. A frame's read-back completes a frame or two after its
/// submit; a frame that finds every slot taken goes untimed.
const SLOTS: usize = 6;
const FREE: u8 = 0;
const MAPPING: u8 = 1;
const MAPPED: u8 = 2;
const FAILED: u8 = 3;

pub struct GpuFrameTimePlugin;

impl Plugin for GpuFrameTimePlugin {
    fn build(&self, app: &mut App) {
        let latest = Latest::default();
        app.insert_resource(latest.clone()).add_systems(PreUpdate, publish);
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            render
                .insert_resource(latest)
                .add_systems(RenderStartup, create)
                .add_systems(
                    RenderGraph,
                    (start.in_set(RenderGraphSystems::Begin), end.in_set(RenderGraphSystems::Finish)),
                );
        }
    }
}

#[derive(Clone, Copy)]
struct FrameTime {
    busy: f64,
    wait: Option<f64>,
}

/// The newest frame read back, waiting for the main world. One that is not taken in time is replaced.
#[derive(Resource, Clone, Default)]
struct Latest(Arc<Mutex<Option<FrameTime>>>);

struct Slot {
    readback: wgpu::Buffer,
    state: Arc<AtomicU8>,
    frame: u64,
}

#[derive(Resource)]
struct Stamps {
    /// Two timestamps a slot: the frame's first and last.
    queries: wgpu::QuerySet,
    /// Where the queries are resolved, a slot every `QUERY_RESOLVE_BUFFER_ALIGNMENT` bytes.
    resolve: wgpu::Buffer,
    slots: Vec<Slot>,
    ms_per_tick: f64,
    /// This frame's slot; none when every slot is still on its way back.
    current: Option<usize>,
    frame: u64,
    /// The last frame read back and its final timestamp, for the next frame's wait.
    previous: Option<(u64, u64)>,
}

impl Stamps {
    fn new(device: &RenderDevice, queue: &RenderQueue) -> Self {
        let device = device.wgpu_device();
        let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("dx_frame_timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: 2 * SLOTS as u32,
        });
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dx_frame_timestamps_resolve"),
            size: wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT * SLOTS as u64,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let slots = (0..SLOTS)
            .map(|_| Slot {
                readback: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("dx_frame_timestamps_readback"),
                    size: 2 * wgpu::QUERY_SIZE as u64,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
                state: Arc::new(AtomicU8::new(FREE)),
                frame: 0,
            })
            .collect();
        Self {
            queries,
            resolve,
            slots,
            ms_per_tick: f64::from(queue.get_timestamp_period()) / 1e6,
            current: None,
            frame: 0,
            previous: None,
        }
    }
}

/// Starts (and on every new device, restarts) the timers, if the device can time inside encoders.
fn create(mut commands: Commands, device: Res<RenderDevice>, queue: Res<RenderQueue>) {
    let needed = wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    if !device.features().contains(needed) {
        eprintln!("[GPU frame] no timestamp queries inside encoders on this device: no whole-frame GPU time");
        return;
    }
    commands.insert_resource(Stamps::new(&device, &queue));
}

/// The `Begin` set, before the camera passes: takes the frames the GPU has finished, oldest first, so each wait has the
/// end of the frame before it, then writes this frame's first timestamp into a slot's query pair.
fn start(device: Res<RenderDevice>, queue: Res<RenderQueue>, latest: Res<Latest>, stamps: Option<ResMut<Stamps>>) {
    let Some(mut stamps) = stamps else { return };
    let stamps = &mut *stamps;
    while let Some(index) = (0..SLOTS)
        .filter(|&i| stamps.slots[i].state.load(Ordering::Acquire) != FREE)
        .min_by_key(|&i| stamps.slots[i].frame)
    {
        let slot = &stamps.slots[index];
        match slot.state.load(Ordering::Acquire) {
            MAPPED => {
                let (first, last) = {
                    let view = slot.readback.slice(..).get_mapped_range();
                    let stamp = |i: usize| {
                        u64::from_le_bytes(view[8 * i..8 * i + 8].try_into().expect("8 bytes"))
                    };
                    (stamp(0), stamp(1))
                };
                slot.readback.unmap();
                let ms = |ticks: u64| ticks as f64 * stamps.ms_per_tick;
                let wait = match stamps.previous {
                    Some((frame, end)) if frame + 1 == slot.frame => first.checked_sub(end).map(ms),
                    _ => None,
                };
                stamps.previous = Some((slot.frame, last));
                if let (Some(busy), Ok(mut latest)) = (last.checked_sub(first).map(ms), latest.0.lock()) {
                    *latest = Some(FrameTime { busy, wait });
                }
            }
            FAILED => stamps.previous = None,
            _ => break,
        }
        slot.state.store(FREE, Ordering::Relaxed);
    }
    stamps.current = (0..SLOTS).find(|&i| stamps.slots[i].state.load(Ordering::Acquire) == FREE);
    if let Some(index) = stamps.current {
        stamps.slots[index].frame = stamps.frame;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.write_timestamp(&stamps.queries, 2 * index as u32);
        queue.submit([encoder.finish()]);
    }
    stamps.frame += 1;
}

/// The `Finish` set, after the passes are submitted: writes this frame's last timestamp, resolves the pair into the
/// slot's readback, and maps the readback once that copy is queued.
fn end(device: Res<RenderDevice>, queue: Res<RenderQueue>, stamps: Option<ResMut<Stamps>>) {
    let Some(mut stamps) = stamps else { return };
    let Some(index) = stamps.current.take() else { return };
    let first = 2 * index as u32;
    let offset = wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT * index as u64;
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.write_timestamp(&stamps.queries, first + 1);
    encoder.resolve_query_set(&stamps.queries, first..first + 2, &stamps.resolve, offset);
    encoder.copy_buffer_to_buffer(
        &stamps.resolve,
        offset,
        &stamps.slots[index].readback,
        0,
        Some(2 * wgpu::QUERY_SIZE as u64),
    );
    queue.submit([encoder.finish()]);
    let slot = &stamps.slots[index];
    let state = slot.state.clone();
    state.store(MAPPING, Ordering::Relaxed);
    slot.readback.map_async(wgpu::MapMode::Read, .., move |result| {
        state.store(if result.is_ok() { MAPPED } else { FAILED }, Ordering::Release);
    });
}

/// Main world: the newest frame the render world read back, into the diagnostics the profiler prints.
fn publish(latest: Res<Latest>, store: Option<ResMut<DiagnosticsStore>>) {
    let Some(mut store) = store else { return };
    let Some(frame) = latest.0.lock().ok().and_then(|mut latest| latest.take()) else { return };
    let time = Instant::now();
    for (path, value) in [(&BUSY, Some(frame.busy)), (&WAIT, frame.wait)] {
        let Some(value) = value else { continue };
        if store.get(path).is_none() {
            store.add(Diagnostic::new(path.clone()).with_suffix("ms"));
        }
        if let Some(diagnostic) = store.get_mut(path) {
            diagnostic.add_measurement(DiagnosticMeasurement { time, value });
        }
    }
}
