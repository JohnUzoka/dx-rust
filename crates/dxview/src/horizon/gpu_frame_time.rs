//! The GPU's time for a whole frame, beside the per-pass times of `RenderDiagnosticsPlugin`. On with the profiler
//! (`env DXVIEW_PROF=1`), where the device has timestamp queries inside encoders. Ported from Skate 3 Rust Engine's
//! `gpu_frame_time.rs` (Bevy 0.18, render graph nodes). Bevy 0.19 has no render graph nodes: the camera passes run in
//! the `RenderGraph` schedule between its `Begin` and `Finish` sets, so the first timestamp is written by a `Begin`
//! system and the last by a `Finish` system, each in its own command buffer. The queue runs command buffers in
//! submission order, so they bracket the frame's passes and submits the same way.
//!
//! The pass timers cover only their passes, and on the Switch they summed to less than the frame. Two timestamps around
//! the whole graph tell the rest apart:
//!
//! - `render/dx_frame/busy_gpu`: from the frame's first command to its last. Less the sum of the pass times, it is the
//!   work inside the frame that no pass timer covers.
//! - `render/dx_frame/wait_gpu`: from the previous frame's last command to this frame's first. Idle time, plus what
//!   runs between two frames' graphs: the uploads, the screenshot and readback copies, and any wait for the compositor
//!   to release the swapchain image.
//! - `render/dx_frame/passes_gpu`: every pass span of the newest frame added up (`frame_sums`). Bevy makes each span
//!   its own measurement, so a pass that two cameras run prints as the mean of their spans; `busy_gpu` less this is
//!   the work no pass timer covers, and `render/dx_frame/frame/<pass>` is such a pass's sum over the frame.
//!
//! Both are read back a frame or two late, into `DiagnosticsStore`, and `prof.rs` prints their averages with the other
//! `render/` timers. Run 6's log had neither, and nothing in it said why; that console run turned out to have used run
//! 5's NRO, which has no timer. This version still creates its query resources on the first `Begin` (no `RenderStartup`
//! hook), polls the device so mapped readbacks are seen, and reports each step it reaches as one `[GPU frame]` line
//! (`report_once`). The lines are queued by the render world and printed by the main world, whose output the log keeps,
//! and the main world also says if the `Begin` set never runs.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU8, AtomicU32, Ordering},
};

use bevy::{
    diagnostic::{Diagnostic, DiagnosticMeasurement, DiagnosticPath, DiagnosticsStore},
    platform::time::Instant,
    prelude::*,
    render::{
        RenderApp,
        renderer::{RenderDevice, RenderGraph, RenderGraphSystems, RenderQueue},
    },
};

pub(crate) const BUSY: DiagnosticPath = DiagnosticPath::const_new("render/dx_frame/busy_gpu");
pub(crate) const WAIT: DiagnosticPath = DiagnosticPath::const_new("render/dx_frame/wait_gpu");
pub(crate) const PASSES: DiagnosticPath = DiagnosticPath::const_new("render/dx_frame/passes_gpu");

/// Frames whose timestamps can be on their way back at once. A frame's read-back completes a frame or two after its
/// submit; a frame that finds every slot taken goes untimed.
const SLOTS: usize = 6;
const FREE: u8 = 0;
const MAPPING: u8 = 1;
const MAPPED: u8 = 2;
const FAILED: u8 = 3;

/// One `[GPU frame]` line per bit: each step is reported once.
const REPORT_START: u32 = 1 << 0;
const REPORT_UNAVAILABLE: u32 = 1 << 1;
const REPORT_CREATED: u32 = 1 << 2;
const REPORT_FIRST: u32 = 1 << 3;
const REPORT_NEGATIVE: u32 = 1 << 4;
const REPORT_MAP_FAILED: u32 = 1 << 5;
const REPORT_STALL: u32 = 1 << 6;
static REPORTED: AtomicU32 = AtomicU32::new(0);

/// The lines the render world has queued, for the main world to print (statics are shared by both worlds).
static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Queues `[GPU frame] …` the first time `bit` is reported, and never again.
fn report_once(bit: u32, text: impl FnOnce() -> String) {
    if REPORTED.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
        if let Ok(mut lines) = LINES.lock() {
            lines.push(text());
        }
    }
}

pub struct GpuFrameTimePlugin;

impl Plugin for GpuFrameTimePlugin {
    fn build(&self, app: &mut App) {
        let latest = Latest::default();
        app.insert_resource(latest.clone()).add_systems(PreUpdate, publish).add_systems(Update, frame_sums);
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            render.insert_resource(latest).add_systems(
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
    /// Readbacks handed back so far.
    taken: u64,
}

impl Stamps {
    /// The query resources, or `None` when the device can't time inside encoders.
    fn try_new(device: &RenderDevice, queue: &RenderQueue) -> Option<Self> {
        let needed = wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
        if !device.features().contains(needed) {
            report_once(REPORT_UNAVAILABLE, || {
                "no timestamp queries inside encoders on this device: no whole-frame GPU time".to_string()
            });
            return None;
        }
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
        Some(Self {
            queries,
            resolve,
            slots,
            ms_per_tick: f64::from(queue.get_timestamp_period()) / 1e6,
            current: None,
            frame: 0,
            previous: None,
            taken: 0,
        })
    }
}

/// The `Begin` set, before the camera passes: creates the query resources on the first frame, takes the frames the GPU
/// has finished, oldest first (so each wait has the end of the frame before it), then writes this frame's first
/// timestamp into a slot's query pair.
fn start(
    mut commands: Commands,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    latest: Res<Latest>,
    stamps: Option<ResMut<Stamps>>,
) {
    report_once(REPORT_START, || "the Begin set runs; the frame's first timestamp is next".to_string());
    let Some(mut stamps) = stamps else {
        if let Some(new) = Stamps::try_new(&device, &queue) {
            report_once(REPORT_CREATED, || {
                format!("timestamps on: {SLOTS} frames in flight, {:.3} ns per tick", new.ms_per_tick * 1e6)
            });
            commands.insert_resource(new);
        }
        return;
    };
    let stamps = &mut *stamps;
    // Mapped readbacks are handed back when the device is polled; `Poll` doesn't wait.
    let _ = device.wgpu_device().poll(wgpu::PollType::Poll);
    while let Some(index) = (0..SLOTS)
        .filter(|&i| stamps.slots[i].state.load(Ordering::Acquire) != FREE)
        .min_by_key(|&i| stamps.slots[i].frame)
    {
        let slot = &stamps.slots[index];
        match slot.state.load(Ordering::Acquire) {
            MAPPED => {
                let (first, last) = {
                    let view = slot.readback.slice(..).get_mapped_range();
                    let stamp =
                        |i: usize| u64::from_le_bytes(view[8 * i..8 * i + 8].try_into().expect("8 bytes"));
                    (stamp(0), stamp(1))
                };
                slot.readback.unmap();
                stamps.taken += 1;
                report_once(REPORT_FIRST, || format!("first readback: {first} then {last} ticks"));
                let ms = |ticks: u64| ticks as f64 * stamps.ms_per_tick;
                let wait = match stamps.previous {
                    Some((frame, end)) if frame + 1 == slot.frame => first.checked_sub(end).map(ms),
                    _ => None,
                };
                stamps.previous = Some((slot.frame, last));
                match last.checked_sub(first) {
                    Some(ticks) => {
                        if let Ok(mut latest) = latest.0.lock() {
                            *latest = Some(FrameTime { busy: ms(ticks), wait });
                        }
                    }
                    None => report_once(REPORT_NEGATIVE, || {
                        format!("a readback ends before it starts ({first} then {last} ticks): frames not timed")
                    }),
                }
            }
            FAILED => stamps.previous = None,
            _ => break,
        }
        slot.state.store(FREE, Ordering::Relaxed);
    }
    if stamps.taken == 0 && stamps.frame == 300 {
        let states: Vec<u8> = stamps.slots.iter().map(|s| s.state.load(Ordering::Relaxed)).collect();
        report_once(REPORT_STALL, || {
            format!("no readback after 300 frames; slot states {states:?} (0 free, 1 mapping, 2 mapped, 3 failed)")
        });
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
        if let Err(error) = &result {
            report_once(REPORT_MAP_FAILED, || format!("a readback failed to map: {error:?}"));
        }
        state.store(if result.is_ok() { MAPPED } else { FAILED }, Ordering::Release);
    });
}

/// Main world: prints the `[GPU frame]` lines the render world queued (and says if its `Begin` set never ran), then
/// hands the newest frame read back to the diagnostics the profiler prints.
fn publish(latest: Res<Latest>, store: Option<ResMut<DiagnosticsStore>>) {
    static FRAMES: AtomicU32 = AtomicU32::new(0);
    if let Ok(mut lines) = LINES.lock() {
        for line in lines.drain(..) {
            eprintln!("[GPU frame] {line}");
        }
    }
    if FRAMES.fetch_add(1, Ordering::Relaxed) == 300 && REPORTED.load(Ordering::Relaxed) & REPORT_START == 0 {
        eprintln!("[GPU frame] the Begin set has not run after 300 frames: the render world's graph isn't timed");
    }
    let Some(mut store) = store else { return };
    let Some(frame) = latest.0.lock().ok().and_then(|mut latest| latest.take()) else { return };
    let time = Instant::now();
    for (path, value) in [(&BUSY, Some(frame.busy)), (&WAIT, frame.wait)] {
        if let Some(value) = value {
            measure(&mut store, path, time, value);
        }
    }
}

/// Main world, after Bevy's `sync_diagnostics` (PreUpdate). Bevy makes every span of a pass its own measurement, so a
/// pass that two cameras run, the world's and the weapon's, prints as the mean of their spans. This adds up the newest
/// frame's spans: `render/dx_frame/passes_gpu`, all of them, so that `busy_gpu` less it is the work no pass timer
/// covers, and `render/dx_frame/frame/<pass>` for each pass that has had more than one span. Such a pass stays
/// measured every frame after, whatever its spans: run 17's opaque pass went to one span with the weapon's camera off,
/// and its sum, no longer measured, printed the stale 23.5 ms of a few frames with two.
fn frame_sums(
    store: Option<ResMut<DiagnosticsStore>>,
    mut done: Local<Option<Instant>>,
    mut shared_once: Local<std::collections::HashSet<String>>,
) {
    let Some(mut store) = store else { return };
    let is_pass = |d: &&Diagnostic| d.path().as_str().ends_with("/elapsed_gpu");
    // sync_diagnostics stamps every measurement of one frame with the same instant.
    let Some(newest) = store.iter().filter(is_pass).filter_map(|d| d.measurement().map(|m| m.time)).max() else {
        return;
    };
    if *done == Some(newest) {
        return;
    }
    *done = Some(newest);
    let mut all = 0.0;
    let mut shared = Vec::new();
    for diagnostic in store.iter().filter(is_pass) {
        let (sum, spans) = diagnostic
            .measurements()
            .filter(|m| m.time == newest)
            .fold((0.0, 0), |(sum, spans), m| (sum + m.value, spans + 1));
        all += sum;
        let path = diagnostic.path().as_str();
        if spans > 1 && !shared_once.contains(path) {
            shared_once.insert(path.to_string());
        }
        if shared_once.contains(path) {
            let pass = path.trim_start_matches("render/").trim_end_matches("/elapsed_gpu").replace('/', " ");
            shared.push((DiagnosticPath::from_components(["render", "dx_frame", "frame", pass.as_str()]), sum));
        }
    }
    measure(&mut store, &PASSES, newest, all);
    for (path, sum) in &shared {
        measure(&mut store, path, newest, *sum);
    }
}

/// Adds a measurement to the diagnostic at `path`, making the diagnostic first if it is new.
fn measure(store: &mut DiagnosticsStore, path: &DiagnosticPath, time: Instant, value: f64) {
    if store.get(path).is_none() {
        store.add(Diagnostic::new(path.clone()).with_suffix("ms"));
    }
    if let Some(diagnostic) = store.get_mut(path) {
        diagnostic.add_measurement(DiagnosticMeasurement { time, value });
    }
}
