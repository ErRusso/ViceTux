//! Host pipelines are made by workers, not by the title thread that first
//! records a draw with them. Translating a shader and the host driver's
//! compile take from milliseconds to seconds each (structurisation of the
//! biggest compute programs ~0.4 s, some driver compiles 3.4 s), and
//! the title's first world frame wants over 1,200 new pipelines: made
//! on its recording threads, that frame took more than 30 s and the
//! title's GPU watchdog ended the run (decision 0007, "Pipelines made off
//! the title's threads").
//!
//! A first use queues the pipeline and waits for it within a budget per
//! frame, so the cheap ones (menus, one-off passes such as a colour-grading
//! bake) are there at once as before; past the budget a draw or dispatch
//! whose pipeline is not ready is dropped, as an untranslated one is.
//!
//! The default budget is half a second per title frame. A pass the title
//! runs only once and whose pipeline takes longer is lost. Passes over the
//! screen (no vertex input, no depth) have workers and a longer budget of
//! their own ([`screen_budget`]): the title fills its lighting tables with
//! such draws once, at the world's first frame, and behind a world's worth of
//! pipelines they were dropped; that run had no lamps and no diffuse light to
//! its end (run textures-d2, the first after a new translation; Belobug
//! 18e57db8 found the same).

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::Shader;
use super::host::{GraphicsState, HostPipeline};

/// What a host pipeline is made from, and the host pipeline once made.
pub(crate) struct Pending {
    shaders: Vec<Arc<Shader>>,
    pub(crate) graphics: Option<Box<GraphicsState>>,
    queue_state: AtomicU8,
    /// Taken by a worker from [`Queue::slow`].
    was_slow: AtomicBool,
    made: OnceLock<Option<HostPipeline>>,
    /// What identifies it across runs (shader containers and state), and
    /// whether a draw or dispatch has requested it yet.
    key: String,
    used: AtomicBool,
    /// A pass over the screen ([`GraphicsState::screen_pass`]) or a confirmed
    /// opening-scene / bake producer that must never sit behind cold RT shaders.
    screen: bool,
    /// Confirmed first-scene / bake producer prepared early even on a cold cache.
    scene_input: bool,
    /// Why this pipeline is intentionally skipped (e.g. RT resolve without BVH).
    left_out: Option<&'static str>,
    /// Claimed by the thread that makes it; the others wait for it.
    building: AtomicBool,
    /// The worker making it died ([`watchdog`]): it is never made, and
    /// whoever asks for it goes without, as for a pipeline the host refused.
    lost: AtomicBool,
}

fn named(shaders: &[Arc<Shader>], name: &[u8]) -> bool {
    shaders
        .iter()
        .any(|s| s.stage == "compute" && s.container.windows(name.len()).any(|w| w == name))
}

/// The title's ray-traced shadow passes: distant (directional) shadow
/// traces, the shadow mask's checkerboard rays and the laser lights'.
const RT_SHADOW_TRACES: [&[u8]; 3] =
    [b"CS_RTDistantShadowTrace", b"CS_TraceShadowRays", b"CS_LaserLightShadowRaytracing"];

/// Whether [`RT_SHADOW_TRACES`] are left out: under Wine with no BVH built
/// (`VICTORY_BVH_BUILDER` unset), unless `VICTORY_RT_SHADOWS=1`. Once the
/// single-latch translation let vkd3d-proton make them (before, its
/// compiler died on them), they traced an acceleration structure never
/// built, and smoke and haze in lit rooms showed brown and dark blotches
/// (RTX 4060, Proton Experimental 11, the prison canteen). `0` leaves them
/// out everywhere, with a BVH too: they trace every pixel, and an 8 GB card
/// keeps the structure's memory over PCIe (with the builder, 4-8 fps on an
/// RTX 4060); the bounced light that lights rooms stays.
fn skip_rt_shadows() -> bool {
    static SKIP: OnceLock<bool> = OnceLock::new();
    *SKIP.get_or_init(|| {
        let shadows = std::env::var("VICTORY_RT_SHADOWS").ok();
        if shadows.as_deref() == Some("0") {
            return true;
        }
        if std::env::var_os("VICTORY_BVH_BUILDER").is_some_and(|v| v == "1") {
            return false;
        }
        shadows.as_deref() != Some("1") && crate::host::on_wine()
    })
}

fn left_out(shaders: &[Arc<Shader>], skip_hashes: &[String]) -> Option<&'static str> {
    static SKIP_RT_RESOLVE: OnceLock<bool> = OnceLock::new();
    let skip_rt = *SKIP_RT_RESOLVE.get_or_init(|| {
        let builder = std::env::var_os("VICTORY_BVH_BUILDER").is_some_and(|v| v == "1");
        if builder {
            return false;
        }
        // Without BVH, ~1,000 CS_RaytracingResolve shaders shade no hits while
        // clogging the cold compiler queue; skip unless VICTORY_RT_RESOLVE=1.
        std::env::var_os("VICTORY_RT_RESOLVE").is_none_or(|v| v == "0")
    });
    if skip_rt && named(shaders, b"CS_RaytracingResolve") {
        return Some("shades ray-tracing hits without BVH, left out");
    }
    static SKIP_ACTIVATE_RAYS: OnceLock<bool> = OnceLock::new();
    let skip_activate = *SKIP_ACTIVATE_RAYS.get_or_init(|| {
        let builder = std::env::var_os("VICTORY_BVH_BUILDER").is_some_and(|v| v == "1");
        if builder {
            return std::env::var_os("VICTORY_ACTIVATE_RAYS").is_some_and(|v| v == "0");
        }
        std::env::var_os("VICTORY_ACTIVATE_RAYS").is_none_or(|v| v == "0")
    });
    if skip_activate && named(shaders, b"CS_ScreenProbes_ActivateRays") {
        return Some("ray activation shader without BVH, left out");
    }
    if skip_rt_shadows()
        && RT_SHADOW_TRACES.iter().any(|name| named(shaders, name))
    {
        return Some("traces ray-traced shadows without BVH under Wine, left out");
    }
    if let Some(why) = rt_low_left_out(shaders, super::rt_low()) {
        return Some(why);
    }
    if super::skipped_in_list(shaders, skip_hashes).is_some() {
        return Some("skipped by VICTORY_SKIP_PIPELINES");
    }
    None
}

/// With `on` (`VICTORY_RT_LOW=1`, `--rt-low`): the reflection traces
/// (`CS_PPR_Raytrace`, and its fallback) left out, for the time of an 8 GB
/// card; the bounced light keeps its rays.
fn rt_low_left_out(shaders: &[Arc<Shader>], on: bool) -> Option<&'static str> {
    (on && named(shaders, b"CS_PPR_Raytrace")).then_some("traces reflections, left out (VICTORY_RT_LOW)")
}

/// Confirmed first-scene producers. Prepare these at creation even on a cold
/// launch so cutscene lighting, tiling, and bakes never miss their first frame.
fn scene_input(shaders: &[Arc<Shader>]) -> bool {
    shaders.iter().any(|s| {
        matches!(
            s.hash.as_str(),
            "970c4fbb3b017c0e"
                | "068c90badc86f1fd"
                | "e4a7e088c7780ce8"
                | "ebe4e53f031f5249"
                | "2cc4c197ee29e291"
                | "369cbe906ea72764"
        ) || crate::draw::BAKE_PIXEL_SHADERS.contains(&s.hash.as_str())
    })
}

// SAFETY: D3D12 objects are free-threaded; the graphics state (whose input
// elements point into its own names) is only read after creation.
unsafe impl Send for Pending {}
// SAFETY: as above.
unsafe impl Sync for Pending {}

impl Pending {
    pub(crate) fn new(shaders: Vec<Arc<Shader>>, graphics: Option<Box<GraphicsState>>) -> Pending {
        Self::new_with_skip_hashes(shaders, graphics, super::skip_hashes())
    }

    fn new_with_skip_hashes(
        shaders: Vec<Arc<Shader>>,
        graphics: Option<Box<GraphicsState>>,
        skip_hashes: &[String],
    ) -> Pending {
        let mut key = blake3::Hasher::new();
        for s in &shaders {
            key.update(s.stage.as_bytes());
            key.update(s.hash.as_bytes());
        }
        key.update(graphics.as_ref().map(|g| g.describe()).unwrap_or_default().as_bytes());
        let scene_input = scene_input(&shaders);
        let screen = (graphics.as_ref().is_some_and(|g| g.screen_pass())
            && shaders.iter().all(|s| s.stage != "mesh"))
            || scene_input;
        let key = key.finalize().to_hex()[..32].to_owned();
        let left_out = left_out(&shaders, skip_hashes).or_else(|| {
            super::library::lost(&key)
                .then_some("killed a pipeline worker in an earlier run (lost-pipelines.txt)")
        });
        Pending {
            screen,
            scene_input,
            left_out,
            shaders,
            graphics,
            queue_state: AtomicU8::new(NOT_QUEUED),
            was_slow: AtomicBool::new(false),
            made: OnceLock::new(),
            key,
            used: AtomicBool::new(false),
            building: AtomicBool::new(false),
            lost: AtomicBool::new(false),
        }
    }

    pub(super) fn key(&self) -> &str {
        &self.key
    }

    fn make(&self) {
        self.make_with(|| {
            let _building = crate::timeline::span("pipeline: build a host pipeline");
            let began = Instant::now();
            let made = HostPipeline::new(&self.shaders, self.graphics.as_deref());
            super::library::note_time(&self.key, began.elapsed().as_millis() as u64);
            made
        });
    }

    fn make_with(&self, build: impl FnOnce() -> Option<HostPipeline>) {
        // One thread makes it; the others wait for it to be made or lost (a
        // `get_or_init` behind a thread that died would wait for ever). A
        // build that panics is refused; one that kills its thread outright
        // is the watchdog's.
        if self.made.get().is_none() && !self.lost.load(Ordering::Relaxed) {
            if self.building.swap(true, Ordering::AcqRel) {
                let (lock, done) = &DONE;
                let mut held = lock.lock().unwrap_or_else(|e| e.into_inner());
                while self.made.get().is_none() && !self.lost.load(Ordering::Relaxed) {
                    held = done
                        .wait_timeout(held, Duration::from_millis(50))
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
                return;
            }
            self.made.get_or_init(|| match catch_unwind(AssertUnwindSafe(build)) {
                Ok(pipeline) => pipeline,
                Err(_) => {
                    tracing::warn!(key = self.key, shaders = ?self.shaders, "host pipeline build panicked");
                    None
                }
            });
        }
        let (lock, done) = &DONE;
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        done.notify_all();
    }
}

/// Reports why a host pipeline was left out, is pending, or failed to build.
/// Returns `None` when the host pipeline is ready.
#[cfg(feature = "diagnostics")]
pub(crate) fn missing(p: &Pending) -> Option<String> {
    match p.made.get() {
        Some(Some(_)) => None,
        None if p.lost.load(Ordering::Relaxed) => {
            Some("its worker died making it (lost-pipelines.txt)".into())
        }
        None => Some(p.left_out.unwrap_or("being made").into()),
        Some(None) => {
            let failed: Vec<String> = p
                .shaders
                .iter()
                .filter(|s| matches!(s.translated.get(), Some(None)))
                .map(|s| format!("{} {}", s.stage, s.hash))
                .collect();
            Some(if failed.is_empty() {
                format!("refused {:?}", p.shaders)
            } else {
                failed.join(" + ")
            })
        }
    }
}

/// The hashes of the shaders `p` is made of.
pub(crate) fn hashes(p: &Pending) -> String {
    let hashes: Vec<&str> = p.shaders.iter().map(|s| s.hash.as_str()).collect();
    hashes.join("+")
}

/// Signalled whenever a worker has made a pipeline.
static DONE: (Mutex<()>, Condvar) = (Mutex::new(()), Condvar::new());

/// Waiting allowed per title frame, over all threads.
fn wait_budget() -> Duration {
    static BUDGET: OnceLock<Duration> = OnceLock::new();
    *BUDGET.get_or_init(|| {
        let millis = std::env::var("VICTORY_PIPELINE_WAIT_MS")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(500);
        tracing::info!(millis, "host pipeline wait budget per frame");
        Duration::from_millis(millis)
    })
}

/// As [`wait_budget`], for passes over the screen (`VICTORY_SCREEN_WAIT_MS`,
/// 10 s; 0: like the others). Below the title's 30 s watchdog.
fn screen_budget() -> Duration {
    static BUDGET: OnceLock<Duration> = OnceLock::new();
    *BUDGET.get_or_init(|| {
        let millis = std::env::var("VICTORY_SCREEN_WAIT_MS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(10_000);
        Duration::from_millis(millis)
    })
}

/// Whether passes over the screen go before the others.
fn screens_first() -> bool {
    !screen_budget().is_zero()
}

/// Pipelines queued and not made yet.
static WAITING: AtomicU64 = AtomicU64::new(0);

/// Pipelines queued and not made yet (for the frame line).
pub(crate) fn waiting() -> u64 {
    WAITING.load(Ordering::Relaxed)
}

/// The host pipeline if it is made (or is made within the frame's budget);
/// queues it on first use.
pub(crate) fn ready(p: &Arc<Pending>) -> Option<&HostPipeline> {
    if p.left_out.is_some() || p.lost.load(Ordering::Relaxed) {
        return None;
    }
    // A still-pending pipeline was requested by the title too. Prewarm it
    // on the next launch, even if this run never got to use it.
    if !p.used.load(Ordering::Relaxed) && !p.used.swap(true, Ordering::Relaxed) {
        super::library::note_used(&p.key);
    }
    if p.made.get().is_none() {
        start(p, REQUESTED);
        wait(p, crate::frame::current());
    }
    p.made.get()?.as_ref()
}

/// Makes `p` on this thread if it is not made yet (waiting for a worker
/// that is making it); whether it has a host pipeline.
pub(crate) fn now(p: &Arc<Pending>) -> bool {
    if p.left_out.is_some() || p.lost.load(Ordering::Relaxed) {
        return false;
    }
    p.make();
    p.made.get().is_some_and(|made| made.is_some())
}

/// At the title's creation of a pipeline: starts it at once if an earlier
/// run drew with it (during loading, so the world's first frames find it),
/// or if it is a confirmed opening-scene / bake producer.
pub(crate) fn early(p: &Arc<Pending>) {
    if p.left_out.is_some() {
        return;
    }
    if !p.shaders.is_empty() && (p.scene_input || super::library::wanted_early(&p.key)) {
        super::library::EARLY.fetch_add(1, Ordering::Relaxed);
        start(p, if p.scene_input { REQUESTED } else { EARLY });
    }
}

/// Whether the frame's budget is one for all threads together: a thread
/// that waits takes what is left of it and gives back what it did not
/// use. Counted only when a wait had ended, the budget was one per thread:
/// each of the title's five recording threads began its first wait of a
/// frame with nothing counted yet and waited the whole half second, 2.5 s
/// of waiting in every frame in which pipelines were being made (runs
/// 34-49: "2504 ms waiting for pipelines" in each such frame).
/// On by default; `VICTORY_PIPELINE_WAIT_SHARED=0` counts as before.
fn shared_budget() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_PIPELINE_WAIT_SHARED").is_none_or(|v| v != "0"))
}

/// Waits for `p` while frame `frame` has budget left.
fn wait(p: &Pending, frame: u64) {
    let screen = p.screen && screens_first();
    let budget = if screen { screen_budget() } else { wait_budget() };
    if budget.is_zero() {
        return;
    }
    // The frame the budget belongs to, and what was waited of it (or is
    // taken by a thread waiting now); of the passes over the screen too.
    static SPENT: [Mutex<(u64, Duration)>; 2] = [const { Mutex::new((u64::MAX, Duration::ZERO)) }; 2];
    let spent_of = &SPENT[usize::from(screen)];
    let shared = shared_budget();
    let left = {
        let mut spent = spent_of.lock().unwrap_or_else(|e| e.into_inner());
        if spent.0 != frame {
            *spent = (frame, Duration::ZERO);
        }
        let left = budget.saturating_sub(spent.1);
        if shared {
            spent.1 += left;
        }
        left
    };
    if left.is_zero() {
        return;
    }
    let waiting = crate::timeline::span("pipeline: first use waits for a host pipeline");
    let started = Instant::now();
    let (lock, done) = &DONE;
    let mut held = lock.lock().unwrap_or_else(|e| e.into_inner());
    while p.made.get().is_none() && !p.lost.load(Ordering::Relaxed) {
        let rest = left.saturating_sub(started.elapsed());
        if rest.is_zero() {
            break;
        }
        held = done.wait_timeout(held, rest).unwrap_or_else(|e| e.into_inner()).0;
    }
    drop(held);
    let waited = started.elapsed().min(left);
    waiting.end();
    if screen && waited > Duration::from_millis(500) {
        tracing::info!(
            ms = waited.as_millis() as u64,
            made = p.made.get().is_some(),
            shaders = ?p.shaders,
            "a pass over the screen was waited for (VICTORY_SCREEN_WAIT_MS=0: dropped past half a second a frame)"
        );
    }
    {
        let mut spent = spent_of.lock().unwrap_or_else(|e| e.into_inner());
        // A frame that began meanwhile has its own budget.
        if spent.0 == frame {
            if shared {
                spent.1 = spent.1.saturating_sub(left - waited);
            } else {
                spent.1 += waited;
            }
        }
    }
    #[cfg(feature = "diagnostics")]
    crate::copystat::add(
        crate::copystat::C::PipelineWaitUs,
        started.elapsed().as_micros() as u64,
    );
}

const NOT_QUEUED: u8 = 0;
const EARLY: u8 = 1;
const REQUESTED: u8 = 2;
const MAKING: u8 = 3;

/// How many workers may make pipelines at once that are known to take long
/// and that no draw has asked for: a quarter of them. On a fresh cache (a
/// change of the translator or the driver) the early list begins with
/// programs that take tens of seconds each: all 16 workers were in them
/// through the whole of the title's loading, 97 pipelines made in 20 s, and
/// the first minute and a half of the world ran at 0.5 to 1.6 s a frame,
/// dropping up to 2,900 draws a frame (run shaders-1).
/// `VICTORY_PIPELINE_SLOW=0`: in the order the title creates them, as before.
fn slow_limit() -> usize {
    static LIMIT: OnceLock<usize> = OnceLock::new();
    *LIMIT.get_or_init(|| {
        if std::env::var_os("VICTORY_PIPELINE_SLOW").is_some_and(|v| v == "0") {
            usize::MAX
        } else {
            (worker_count(None, std::thread::available_parallelism().map_or(2, |n| n.get())) / 4).max(1)
        }
    })
}

struct Queue {
    /// Requested passes over the screen: before all others, and the only ones
    /// [`SCREEN_WORKERS`] take.
    screen: VecDeque<Arc<Pending>>,
    early: VecDeque<Arc<Pending>>,
    /// Those of the early ones known to take long, and how many of them are
    /// being made ([`slow_limit`]).
    slow: VecDeque<Arc<Pending>>,
    slow_now: usize,
    requested: VecDeque<Arc<Pending>>,
}

impl Queue {
    const fn new() -> Self {
        Self {
            screen: VecDeque::new(),
            early: VecDeque::new(),
            slow: VecDeque::new(),
            slow_now: 0,
            requested: VecDeque::new(),
        }
    }

    /// Queues an unrequested pipeline or promotes one queued at creation.
    /// Returns whether it is new to the queue, or `None` if nothing changed.
    fn push(&mut self, p: &Arc<Pending>, priority: u8) -> Option<bool> {
        self.push_as(p, priority, slow_limit() != usize::MAX && super::library::slow(&p.key))
    }

    fn push_as(&mut self, p: &Arc<Pending>, priority: u8, slow: bool) -> Option<bool> {
        let prev = p.queue_state.load(Ordering::Relaxed);
        if prev >= priority {
            return None;
        }
        p.queue_state.store(priority, Ordering::Relaxed);
        if priority == REQUESTED && p.screen && screens_first() {
            self.screen.push_back(p.clone());
        } else if priority == REQUESTED {
            self.requested.push_back(p.clone());
        } else if slow {
            self.slow.push_back(p.clone());
        } else {
            self.early.push_back(p.clone());
        }
        Some(prev == NOT_QUEUED)
    }

    fn pop(&mut self, screens_only: bool, slow_limit: usize) -> Option<Arc<Pending>> {
        // An early entry can have been promoted and claimed already.
        let unclaimed = |list: &mut VecDeque<Arc<Pending>>| {
            std::iter::from_fn(|| list.pop_front()).find(|p| p.queue_state.load(Ordering::Relaxed) == EARLY)
        };
        let mut p = self.screen.pop_front();
        if p.is_none() && !screens_only {
            p = self.requested.pop_front().or_else(|| unclaimed(&mut self.early));
            if p.is_none() && self.slow_now < slow_limit {
                p = unclaimed(&mut self.slow);
                if let Some(p) = &p {
                    p.was_slow.store(true, Ordering::Relaxed);
                    self.slow_now += 1;
                }
            }
        }
        let p = p?;
        p.queue_state.store(MAKING, Ordering::Relaxed);
        Some(p)
    }
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue::new());
static AVAILABLE: Condvar = Condvar::new();

/// Start or promote work for a draw without waiting behind the entire early
/// list. Queue changes and worker claims happen under the same lock.
fn start(p: &Arc<Pending>, priority: u8) {
    if p.queue_state.load(Ordering::Relaxed) >= priority {
        return;
    }
    if !workers() {
        if p.queue_state.swap(MAKING, Ordering::Relaxed) == NOT_QUEUED {
            p.make();
        }
        return;
    }
    let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(new) = queue.push(p, priority) {
        if new {
            PEAK.fetch_max(WAITING.fetch_add(1, Ordering::Relaxed) + 1, Ordering::Relaxed);
        }
        // (One woken could be a worker that takes passes over the screen
        // alone: it would leave the pipeline to nobody.)
        AVAILABLE.notify_all();
    }
}

/// Workers that make passes over the screen alone, besides the others.
const SCREEN_WORKERS: usize = 2;

fn next(screens_only: bool) -> Arc<Pending> {
    let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if let Some(p) = queue.pop(screens_only, slow_limit()) {
            return p;
        }
        queue = AVAILABLE.wait(queue).unwrap_or_else(|e| e.into_inner());
    }
}

/// How many workers make pipelines: `asked` (`VICTORY_PIPELINE_WORKERS`, 1 to
/// 64), else two thirds of the logical processors, 2 to 16. They run below the
/// title's own threads, and a cold cache queues hundreds at once.
fn worker_count(asked: Option<usize>, processors: usize) -> usize {
    worker_count_for(asked, processors, false)
}

fn worker_count_for(asked: Option<usize>, processors: usize, limited_ram: bool) -> usize {
    match asked {
        Some(n) => n.clamp(1, 64),
        None if limited_ram => (processors * 3 / 4).clamp(2, 8),
        None => (processors * 2 / 3).clamp(2, 16),
    }
}

/// Each worker's stack: Rust's default unless `VICTORY_PIPELINE_STACK_MB`
/// asks for another size (a larger one did not help the pipelines that
/// kill workers under Proton: vkd3d-proton recursed without end on them,
/// through 1 GiB; see [`watchdog`]).
fn worker_stack() -> Option<usize> {
    std::env::var("VICTORY_PIPELINE_STACK_MB")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&mb| (2..=4096).contains(&mb))
        .map(|mb| mb << 20)
}

/// Starts the workers ([`worker_count`]) on first use, and the
/// [`watchdog`] that replaces one that dies.
fn workers() -> bool {
    static RUNNING: OnceLock<bool> = OnceLock::new();
    *RUNNING.get_or_init(|| {
        let mut running = false;
        let workers = worker_count_for(
            std::env::var("VICTORY_PIPELINE_WORKERS")
                .ok()
                .and_then(|v| v.trim().parse().ok()),
            std::thread::available_parallelism().map_or(2, |n| n.get()),
            crate::residency::limited_host_ram(),
        );
        tracing::info!(workers, "pipeline workers");
        let screens = if screens_first() { SCREEN_WORKERS } else { 0 };
        for i in 0..workers + screens {
            running |= spawn_worker(i, i >= workers);
        }
        if running {
            watchdog();
        }
        running
    })
}

/// A worker of ours: its thread (a handle of its own, to tell it has
/// ended), what it takes, and the pipeline it is making.
struct Worker {
    thread: usize,
    index: usize,
    screens_only: bool,
    making: Option<Arc<Pending>>,
}

static WORKERS: Mutex<Vec<Worker>> = Mutex::new(Vec::new());

/// Whether worker `index` started.
fn spawn_worker(index: usize, screens_only: bool) -> bool {
    let mut builder = std::thread::Builder::new().name(format!("victory-pipelines-{index}"));
    if let Some(size) = worker_stack() {
        builder = builder.stack_size(size);
    }
    let spawned = builder.spawn(move || {
        use windows::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
        use windows::Win32::System::Threading::*;
        // Below the title's own threads (loading, recording),
        // and the streaming one, which is below normal itself
        // (`VICTORY_PIPELINE_LOWEST=0`: level with it, as before).
        let lowest =
            !screens_only && std::env::var_os("VICTORY_PIPELINE_LOWEST").is_none_or(|v| v != "0");
        let mut thread = HANDLE::default();
        // SAFETY: plain calls on our own thread and process.
        let duplicated = unsafe {
            let priority = if lowest {
                THREAD_PRIORITY_LOWEST
            } else {
                THREAD_PRIORITY_BELOW_NORMAL
            };
            let _ = SetThreadPriority(GetCurrentThread(), priority);
            DuplicateHandle(
                GetCurrentProcess(),
                GetCurrentThread(),
                GetCurrentProcess(),
                &mut thread,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
            .is_ok()
        };
        let slot = |f: &mut dyn FnMut(&mut Worker)| {
            let mut workers = WORKERS.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(w) = workers.iter_mut().find(|w| w.thread == thread.0 as usize) {
                f(w);
            }
        };
        if duplicated {
            WORKERS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(Worker {
                    thread: thread.0 as usize,
                    index,
                    screens_only,
                    making: None,
                });
        }
        loop {
            let p = next(screens_only);
            slot(&mut |w| w.making = Some(p.clone()));
            p.make();
            slot(&mut |w| w.making = None);
            finished(&p);
        }
    });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "no pipeline worker");
        return false;
    }
    true
}

/// A pipeline taken from the queue is made, or lost.
fn finished(p: &Pending) {
    if p.was_slow.load(Ordering::Relaxed) {
        let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
        queue.slow_now -= 1;
        AVAILABLE.notify_all();
    }
    if WAITING.fetch_sub(1, Ordering::Relaxed) == 1 {
        drained();
    } else {
        super::library::save(false);
    }
}

/// Looks at the workers every quarter of a second. One whose thread has
/// ended died making a pipeline (under Proton, vkd3d-proton recursing
/// until the stack ran out, `library::LOST_FILE`): that pipeline is lost,
/// written down so later starts leave it out, and another worker takes the
/// dead one's place. Before this, the dead workers' pipelines stayed
/// "being made" for the whole run and the queue behind them with them.
fn watchdog() {
    let _ = std::thread::Builder::new().name("victory-pipelines-watch".into()).spawn(|| {
        use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::WaitForSingleObject;
        let mut replaced = 0u32;
        loop {
            std::thread::sleep(Duration::from_millis(250));
            let dead: Vec<Worker> = {
                let mut workers = WORKERS.lock().unwrap_or_else(|e| e.into_inner());
                let mut dead = Vec::new();
                let mut i = 0;
                while i < workers.len() {
                    // SAFETY: a handle of ours, closed only below.
                    if unsafe { WaitForSingleObject(HANDLE(workers[i].thread as *mut _), 0) } == WAIT_OBJECT_0 {
                        dead.push(workers.swap_remove(i));
                    } else {
                        i += 1;
                    }
                }
                dead
            };
            for w in dead {
                // SAFETY: the handle the worker duplicated for us.
                let _ = unsafe { CloseHandle(HANDLE(w.thread as *mut _)) };
                if let Some(p) = &w.making {
                    p.lost.store(true, Ordering::Relaxed);
                    super::library::note_lost(&p.key);
                    tracing::warn!(
                        worker = w.index,
                        key = %p.key,
                        shaders = %hashes(p),
                        "a pipeline worker died making a pipeline: left out from now on (lost-pipelines.txt), the worker replaced"
                    );
                    {
                        let (lock, done) = &DONE;
                        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
                        done.notify_all();
                    }
                    finished(p);
                } else {
                    tracing::warn!(worker = w.index, "a pipeline worker ended between pipelines: replaced");
                }
                // (A worker that keeps dying is not replaced past this.)
                replaced += 1;
                if replaced <= 64 {
                    spawn_worker(w.index, w.screens_only);
                }
            }
        }
    });
}

/// The most pipelines waiting at once since the workers last ran out.
static PEAK: AtomicU64 = AtomicU64::new(0);

/// The workers ran out of work: logged after a batch of 10 or more, and
/// the cache saved (at most every 20 s).
fn drained() {
    use super::library::{EARLY, LOADED, MADE};
    if PEAK.swap(0, Ordering::Relaxed) >= 10 {
        tracing::info!(
            made = MADE.load(Ordering::Relaxed),
            loaded = LOADED.load(Ordering::Relaxed),
            started_early = EARLY.load(Ordering::Relaxed),
            "host pipelines all made"
        );
    }
    super::library::save(false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflection_traces_are_left_out_with_rt_low() {
        let traces = [crate::pipeline::shader("compute", b"..CS_PPR_RaytraceFallback_1234..")];
        assert_eq!(rt_low_left_out(&traces, true), Some("traces reflections, left out (VICTORY_RT_LOW)"));
        assert_eq!(rt_low_left_out(&traces, false), None);
        let other = [crate::pipeline::shader("compute", b"..CS_CombinedReflections..")];
        assert_eq!(rt_low_left_out(&other, true), None);
    }

    #[test]
    fn a_panicking_pipeline_is_refused_and_the_worker_continues() {
        let failed = Arc::new(Pending::new(Vec::new(), None));
        let next = Arc::new(Pending::new(Vec::new(), None));
        let failed_job = Arc::clone(&failed);
        let next_job = Arc::clone(&next);
        std::thread::spawn(move || {
            failed_job.make_with(|| panic!("synthetic pipeline build panic"));
            next_job.make_with(|| None);
        })
        .join()
        .expect("the pipeline worker should survive");

        assert!(matches!(failed.made.get(), Some(None)));
        assert!(matches!(next.made.get(), Some(None)));
        let mut rebuilt = false;
        failed.make_with(|| {
            rebuilt = true;
            None
        });
        assert!(!rebuilt);
    }

    #[cfg(feature = "diagnostics")]
    #[test]
    fn missing_distinguishes_omitted_and_pending_pipelines() {
        let reason = "ray activation shader without BVH, left out";
        let mut omitted = Pending::new(Vec::new(), None);
        omitted.left_out = Some(reason);
        assert_eq!(missing(&omitted).as_deref(), Some(reason));

        let pending = Pending::new(Vec::new(), None);
        pending.queue_state.store(MAKING, Ordering::Relaxed);
        assert_eq!(missing(&pending).as_deref(), Some("being made"));
    }

    #[test]
    fn worker_count_follows_the_processors_and_the_switch() {
        assert_eq!(worker_count(None, 1), 2);
        assert_eq!(worker_count(None, 24), 16);
        assert_eq!(worker_count(None, 64), 16);
        assert_eq!(worker_count(Some(0), 24), 1);
        assert_eq!(worker_count(Some(20), 24), 20);
        assert_eq!(worker_count_for(None, 8, true), 6);
        assert_eq!(worker_count_for(None, 16, true), 8);
        assert_eq!(worker_count_for(None, 24, true), 8);
        assert_eq!(worker_count_for(Some(12), 8, true), 12);
    }

    /// A first use queues the pipeline and, within the budget, gets the
    /// worker's answer (here: no host pipeline for no shaders).
    #[test]
    fn first_use_is_made_by_a_worker() {
        let p = Arc::new(Pending::new(Vec::new(), None));
        assert!(ready(&p).is_none());
        assert_ne!(p.queue_state.load(Ordering::Relaxed), NOT_QUEUED);
        let started = Instant::now();
        while p.made.get().is_none() && started.elapsed() < Duration::from_secs(10) {
            std::thread::yield_now();
        }
        assert!(matches!(p.made.get(), Some(None)));
    }

    #[test]
    fn pending_pipeline_is_recorded_for_the_next_launch() {
        let mut p = Pending::new(Vec::new(), None);
        p.key = "pending-pipeline-test".to_owned();
        // Simulate a worker that has taken the job but cannot finish it yet.
        p.queue_state.store(MAKING, Ordering::Relaxed);
        let p = Arc::new(p);

        assert!(!super::super::library::wanted_early(&p.key));
        assert!(ready(&p).is_none());
        assert!(p.made.get().is_none());
        assert!(super::super::library::wanted_early(&p.key));
    }

    #[test]
    fn draws_promote_early_pipelines_without_claiming_them_twice() {
        let earlier = Arc::new(Pending::new(Vec::new(), None));
        let promoted = Arc::new(Pending::new(Vec::new(), None));
        let requested = Arc::new(Pending::new(Vec::new(), None));
        let mut queue = Queue::new();

        assert_eq!(queue.push(&earlier, EARLY), Some(true));
        assert_eq!(queue.push(&promoted, EARLY), Some(true));
        assert_eq!(queue.push(&requested, REQUESTED), Some(true));
        assert_eq!(queue.push(&promoted, REQUESTED), Some(false));
        assert_eq!(queue.push(&promoted, REQUESTED), None);

        assert!(Arc::ptr_eq(&queue.pop(false, 1).unwrap(), &requested));
        assert!(Arc::ptr_eq(&queue.pop(false, 1).unwrap(), &promoted));
        assert_eq!(queue.push(&promoted, REQUESTED), None);
        assert!(Arc::ptr_eq(&queue.pop(false, 1).unwrap(), &earlier));
        assert!(queue.pop(false, 1).is_none());
    }

    #[test]
    fn pipelines_known_to_take_long_leave_workers_for_the_others() {
        let new = || Arc::new(Pending::new(Vec::new(), None));
        let (long, longer, asked, quick) = (new(), new(), new(), new());
        let mut queue = Queue::new();
        for p in [&long, &longer, &asked] {
            assert_eq!(queue.push_as(p, EARLY, true), Some(true));
        }
        assert_eq!(queue.push_as(&quick, EARLY, false), Some(true));
        // The quick one first, then one long one at a time; one a draw asks
        // for waits for nobody.
        assert!(Arc::ptr_eq(&queue.pop(false, 1).unwrap(), &quick));
        assert!(Arc::ptr_eq(&queue.pop(false, 1).unwrap(), &long));
        assert!(queue.pop(false, 1).is_none());
        assert_eq!(queue.push_as(&asked, REQUESTED, true), Some(false));
        assert!(Arc::ptr_eq(&queue.pop(false, 1).unwrap(), &asked));
        assert!(!asked.was_slow.load(Ordering::Relaxed));
        assert!(queue.pop(false, 1).is_none());
        queue.slow_now -= 1;
        assert!(Arc::ptr_eq(&queue.pop(false, 1).unwrap(), &longer));
        assert!(queue.pop(false, 1).is_none());
    }

    /// Passes over the screen go before all others, and workers of their own
    /// take nothing else.
    #[test]
    fn passes_over_the_screen_go_first() {
        let of = |screen| {
            let mut p = Pending::new(Vec::new(), None);
            p.screen = screen;
            Arc::new(p)
        };
        let (world, screen) = (of(false), of(true));
        let mut queue = Queue::new();
        assert_eq!(queue.push(&world, REQUESTED), Some(true));
        assert_eq!(queue.push(&screen, REQUESTED), Some(true));
        assert!(Arc::ptr_eq(&queue.pop(true, 1).unwrap(), &screen));
        assert!(queue.pop(true, 1).is_none());
        assert!(Arc::ptr_eq(&queue.pop(false, 1).unwrap(), &world));
    }

    #[test]
    fn skipped_hash_leaves_pipeline_unqueued_and_unmatched_queues_normally() {
        let shader = |hash: &str| {
            Arc::new(Shader {
                stage: "compute",
                hash: hash.to_owned(),
                container: Vec::new(),
                ambient: None,
                probe: None,
                translated: OnceLock::from(None),
            })
        };
        let skips = super::super::parse_skip_hashes("32ed072b9f845048");

        let skipped = Arc::new(Pending::new_with_skip_hashes(
            vec![shader("32ed072b9f845048")],
            None,
            &skips,
        ));
        assert_eq!(skipped.left_out, Some("skipped by VICTORY_SKIP_PIPELINES"));
        early(&skipped);
        assert!(ready(&skipped).is_none());
        assert_eq!(skipped.queue_state.load(Ordering::Relaxed), NOT_QUEUED);
        assert!(skipped.made.get().is_none());
        #[cfg(feature = "diagnostics")]
        assert_eq!(
            missing(&skipped).as_deref(),
            Some("skipped by VICTORY_SKIP_PIPELINES")
        );

        let normal = Arc::new(Pending::new_with_skip_hashes(
            vec![shader("0011223344556677")],
            None,
            &skips,
        ));
        assert_eq!(normal.left_out, None);
        assert!(ready(&normal).is_none());
        assert_ne!(normal.queue_state.load(Ordering::Relaxed), NOT_QUEUED);
        let started = Instant::now();
        while normal.made.get().is_none() && started.elapsed() < Duration::from_secs(10) {
            std::thread::yield_now();
        }
        assert!(matches!(normal.made.get(), Some(None)));
    }
}
