//! Measures what a **warm** WGC capture session costs while it is held open,
//! and what answering off one costs when the moment arrives.
//!
//! # The decision this exists to serve
//!
//! Task **1.9f** holds a WGC session open per monitor while PLACEMENT is
//! visible, so `Ctrl+Space` becomes a *readback* rather than a *capture*. The
//! roadmap row is explicit that the default is owed a measurement and must not
//! be assumed: **if the warm path is cheap at idle it should simply be the
//! default, and the settings gate is complexity nobody needed.**
//!
//! Two recorded arguments point opposite ways, which is why this is a
//! measurement and not a discussion:
//!
//! - **F-29 rejected persistent sessions** for one-shot capture, against
//!   `quality-bars.md` §1's `CPU — overlay visible, passive only` and 80 MB
//!   idle-RAM rows — but it assumed the retained frame is a **continuous CPU
//!   copy**, ~33 MB per monitor of system RAM per frame.
//! - **`UT-F-41` measured WGC delivering 2 frames in 8.25 s on a static
//!   desktop** against 408 with video playing. So the expensive case is *video
//!   playing while the overlay is up*, not *the overlay is up*.
//!
//! This program retains the latest frame as a **GPU texture** (`CopyResource`
//! into a `D3D11_USAGE_DEFAULT` texture we own) and reads back to the CPU only
//! when a shot asks. That is the design F-29 did not price: the per-frame cost
//! is a GPU-to-GPU copy rather than a PCIe transfer into system RAM, and the
//! retained bytes are **VRAM**, which is not what §1's RAM row bounds. Whether
//! that actually shows up as cheap is the question.
//!
//! # Running it
//!
//! ```text
//! cargo run --release -p uptake-capture --example warm_session
//! cargo run --release -p uptake-capture --example warm_session -- --cold
//! ```
//!
//! Options: `--seconds N` (cost window, default 8), `--shots N` (simulated
//! keypresses, default 5), `--cold` (the control — see below), `--monitors N`
//! (hold the first N enumerated instead of every one, default all).
//!
//! **`--monitors 1` is what ADR-0026's third amendment needs.** That amendment
//! flips the warm path to the default only if the *narrowed* configuration lands
//! at or under **+0.25 pp** still and **under +0.40 pp** video, measured with
//! this instrument and against the same two conditions that produced the
//! whole-desktop +0.62 / +0.94 pp. Until 2026-08-08 this program could only hold
//! every monitor, so that condition could not be run at all and dividing four by
//! four is the arithmetic model the amendment explicitly refuses (`F-39`,
//! `UT-F-53`). Run the same four-run matrix below with `--monitors 1`.
//!
//! **Run it four times.** The conditions are the measurement, and no single run
//! answers anything:
//!
//! | # | Command | What it establishes |
//! | - | ------- | ------------------- |
//! | 1 | `--cold`, static desktop | The floor. No sessions held, so cost is the process doing nothing, and each shot pays today's full capture. |
//! | 2 | default, static desktop | **The row the decision turns on.** Cost of holding sessions with nothing moving — the ordinary case of an overlay being up. |
//! | 3 | default, **video playing** | The expensive case `UT-F-41` predicts, and the positive control proving the sessions are live at all. |
//! | 4 | `--cold`, video playing | Isolates the video's own cost from ours. Without it, run 3's number is the machine's, not the feature's. |
//!
//! Run 4 is the one it is tempting to skip. Do not: playing video costs CPU by
//! itself, and run 3 minus run 1 would charge all of that to the warm sessions.
//!
//! # How this is falsifiable, which is the part that matters
//!
//! `I-11` is this project's standing example of an instrument whose silence was
//! indistinguishable from working, and `UT-F-41` is one that reported the
//! opposite of its own data. So:
//!
//! - **Every session prints a positive armed signal** before any shot runs —
//!   frame count, time to first frame, and the age of what it is holding. A
//!   session that retained nothing says so in those words and its shots are
//!   reported as failures, never as fast zeroes.
//! - **`--cold` is the break-the-world control.** It holds no sessions and each
//!   shot takes today's `capture_region` path. If warm and cold report the same
//!   shot latency, the warm path is not doing what this program claims and the
//!   numbers are void. Expect cold shots at roughly the ~383–483 ms this
//!   project has already measured for a whole-desktop freeze.
//! - **System-wide CPU is reported beside our own.** A warm session can move
//!   work into `dwm.exe`, which a process-local number would miss entirely and
//!   report as free.
//!
//! # What this program does *not* answer
//!
//! **Lateness.** `quality-bars.md` §1's fidelity row — *the moment the frozen
//! pixels represent* — is measured against an on-screen millisecond stopwatch,
//! by eye, because a timestamp cannot distinguish "held frame is 8 s old" from
//! "held frame is 8 s old **and pixel-identical to the screen**", and on a
//! static desktop a correct warm implementation reports exactly that. This
//! program reports the held frame's *age*, which is an input to that question
//! and not an answer to it. See that row's footnote before quoting an age as a
//! fidelity result.

#[cfg(windows)]
#[allow(clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC,
        D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, ID3D11Device, ID3D11DeviceContext,
        ID3D11Multithread, ID3D11Texture2D,
    };
    use windows::core::Interface;
    use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
    use windows_capture::frame::Frame;
    use windows_capture::graphics_capture_api::InternalCaptureControl;
    use windows_capture::monitor::Monitor;
    use windows_capture::settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    };
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentThreadId, GetProcessTimes, GetSystemTimes,
    };
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SM_CXVIRTUALSCREEN,
        SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, WM_QUIT, WM_USER,
    };

    // ---------------------------------------------------------------------
    // The retained frame
    // ---------------------------------------------------------------------

    /// One monitor's GPU-side state: the texture holding the latest frame, and
    /// the pre-allocated staging texture a shot reads it back through.
    ///
    /// **The staging texture is allocated up front on purpose**, before the
    /// cost window opens, because that is what a real implementation does — and
    /// it means its ~`w × h × 4` bytes of CPU-accessible memory are *counted*
    /// in the RAM figure rather than hidden behind the shot that allocates it.
    struct Retained {
        context: ID3D11DeviceContext,
        /// GPU-only copy of the newest frame. Never mapped; `CopyResource`
        /// target on arrival, `CopyResource` source on a shot.
        live: ID3D11Texture2D,
        staging: ID3D11Texture2D,
        width: u32,
        height: u32,
    }

    // SAFETY: D3D11 device contexts are not thread-safe by default, and this
    // type is written from the pump thread (on frame arrival) and read from the
    // main thread (on a shot). `retain` calls
    // `ID3D11Multithread::SetMultithreadProtected(TRUE)` on this very context
    // before the value is ever published, which is the documented mechanism for
    // making the immediate context callable from multiple threads — D3D11 then
    // serialises internally. The `Mutex` around the value orders our own
    // accesses on top of that; the `unsafe impl` is needed only because the raw
    // COM pointers are not `Send` in windows-rs, not because the calls race.
    unsafe impl Send for Retained {}

    /// Everything one monitor's session shares between its pump thread and the
    /// main thread.
    struct Slot {
        label: String,
        handle: isize,
        retained: Mutex<Option<Retained>>,
        /// Every frame's arrival time — count, age and gaps all derive from it.
        arrivals: Mutex<Vec<Instant>>,
        /// Set to ask the pump to stop at its next frame; `WM_QUIT` is what
        /// actually unwinds a pump that is never going to get another one.
        stop: AtomicBool,
        /// Reported by the pump thread once its message queue exists.
        thread_id: Mutex<Option<u32>>,
        /// Why this session has nothing retained, if it has nothing retained.
        failure: Mutex<Option<String>>,
    }

    impl Slot {
        fn note_failure(&self, reason: String) {
            let mut guard = self.failure.lock().unwrap_or_else(PoisonError::into_inner);
            if guard.is_none() {
                *guard = Some(reason);
            }
        }

        fn arrival_times(&self) -> Vec<Instant> {
            self.arrivals
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }

    /// Creates a texture matching `src`'s dimensions and format with the given
    /// usage — `CopyResource` requires both to agree exactly.
    fn create_texture(
        device: &ID3D11Device,
        src: &D3D11_TEXTURE2D_DESC,
        usage: windows::Win32::Graphics::Direct3D11::D3D11_USAGE,
        cpu_access: u32,
    ) -> Result<ID3D11Texture2D, String> {
        let desc = D3D11_TEXTURE2D_DESC {
            Usage: usage,
            BindFlags: 0,
            CPUAccessFlags: cpu_access,
            MiscFlags: 0,
            ..*src
        };
        let mut texture = None;
        // SAFETY: `desc` is a fully initialised description and `texture` is a
        // valid out-param; D3D11 reports failure through the HRESULT.
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }
            .map_err(|error| format!("could not create a texture: {error}"))?;
        texture.ok_or_else(|| "CreateTexture2D succeeded but returned nothing".to_string())
    }

    /// Copies the arriving frame into this slot's retained GPU texture,
    /// creating the GPU state on the first frame.
    fn retain(slot: &Slot, frame: &mut Frame<'_>) -> Result<(), String> {
        let mut guard = slot.retained.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.is_none() {
            let src = *frame.desc();
            let device = frame.device().clone();
            let context = frame.device_context().clone();

            // Must happen before the context is published to the main thread —
            // see the `unsafe impl Send` above, which is only sound because of
            // this call.
            let multithread: ID3D11Multithread = context
                .cast()
                .map_err(|error| format!("no ID3D11Multithread on the context: {error}"))?;
            // SAFETY: no memory-safety preconditions. The BOOL is the *previous*
            // mode, which we have no use for — what matters is that protection
            // is on from here, before the context is published to another thread.
            let _previously = unsafe { multithread.SetMultithreadProtected(true) };

            let live = create_texture(&device, &src, D3D11_USAGE_DEFAULT, 0)?;
            let staging = create_texture(
                &device,
                &src,
                D3D11_USAGE_STAGING,
                D3D11_CPU_ACCESS_READ.0.cast_unsigned(),
            )?;
            *guard = Some(Retained {
                context,
                live,
                staging,
                width: src.Width,
                height: src.Height,
            });
        }

        let Some(retained) = guard.as_ref() else {
            return Err("internal: retained state missing right after creation".into());
        };
        // The whole point: GPU to GPU. Nothing crosses to system RAM until a
        // shot asks, which is the cost F-29 priced as continuous and this
        // design does not pay.
        // SAFETY: both textures share dimensions and format by construction
        // (`create_texture` copies them from the frame's own description).
        unsafe {
            retained
                .context
                .CopyResource(&retained.live, frame.as_raw_texture());
        }
        Ok(())
    }

    /// Reads the retained frame back to system RAM, exactly as a `Ctrl+Space`
    /// would. Returns the bytes produced.
    fn readback(slot: &Slot) -> Result<usize, String> {
        let guard = slot.retained.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(retained) = guard.as_ref() else {
            return Err("nothing retained".into());
        };

        // SAFETY: staging and live agree on dimensions and format; the staging
        // texture was created with CPU read access, which is what `Map`
        // requires.
        unsafe {
            retained
                .context
                .CopyResource(&retained.staging, &retained.live);
        }

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: subresource 0 exists on a non-mipped 2D texture, and
        // `mapped` is a valid out-param.
        unsafe {
            retained
                .context
                .Map(&retained.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        }
        .map_err(|error| format!("could not map the staging texture: {error}"))?;

        let width = retained.width as usize;
        let height = retained.height as usize;
        let row_bytes = width * 4;
        let mut out = vec![0u8; row_bytes * height];
        // SAFETY: the mapping is valid until `Unmap`; each source row starts at
        // `RowPitch` bytes into the previous one and is at least `row_bytes`
        // long, and the destination slice is sized to match exactly.
        unsafe {
            let base = mapped.pData.cast::<u8>();
            for y in 0..height {
                let source = base.add(y * mapped.RowPitch as usize);
                let start = y * row_bytes;
                std::ptr::copy_nonoverlapping(source, out[start..].as_mut_ptr(), row_bytes);
            }
            retained.context.Unmap(&retained.staging, 0);
        }
        Ok(out.len())
    }

    // ---------------------------------------------------------------------
    // The capture handler
    // ---------------------------------------------------------------------

    struct WarmFlags {
        slot: Arc<Slot>,
    }

    struct Warm {
        flags: WarmFlags,
    }

    impl GraphicsCaptureApiHandler for Warm {
        type Flags = WarmFlags;
        type Error = String;

        fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
            Ok(Self { flags: ctx.flags })
        }

        fn on_frame_arrived(
            &mut self,
            frame: &mut Frame<'_>,
            capture_control: InternalCaptureControl,
        ) -> Result<(), Self::Error> {
            if self.flags.slot.stop.load(Ordering::Relaxed) {
                capture_control.stop();
                return Ok(());
            }
            let now = Instant::now();
            if let Err(reason) = retain(&self.flags.slot, frame) {
                self.flags.slot.note_failure(reason.clone());
                capture_control.stop();
                return Err(reason);
            }
            self.flags.arrivals_push(now);
            Ok(())
        }

        fn on_closed(&mut self) -> Result<(), Self::Error> {
            // Deliberately does NOT decrement the live-session count. The first
            // smoke run of this program reported "4 sessions still live" after
            // teardown because the count was decremented here — and `WM_QUIT`
            // unwinds the pump's message loop *without* the session closing
            // through the API, so this never ran. The instrument was reporting
            // a failure to stop that had not happened. The count is now
            // decremented where a session is provably over: when the pump
            // thread's closure returns. Same family as `UT-F-41`.
            Ok(())
        }
    }

    impl WarmFlags {
        fn arrivals_push(&self, at: Instant) {
            self.slot
                .arrivals
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(at);
        }
    }

    // ---------------------------------------------------------------------
    // Cost sampling
    // ---------------------------------------------------------------------

    const fn filetime_to_100ns(value: FILETIME) -> u64 {
        ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64
    }

    /// A point-in-time reading of everything the cost windows compare.
    #[derive(Clone, Copy)]
    struct CostSample {
        at: Instant,
        /// Our process's kernel + user time.
        process_100ns: u64,
        /// Machine-wide busy time (all cores), and the total it is out of.
        system_busy_100ns: u64,
        working_set: usize,
        private: usize,
    }

    fn sample_cost() -> Result<CostSample, String> {
        let mut creation = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut exit = creation;
        let mut kernel = creation;
        let mut user = creation;
        // SAFETY: all four are valid out-params and the pseudo-handle from
        // GetCurrentProcess is always valid.
        let ok = unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        };
        if ok == 0 {
            return Err("GetProcessTimes failed".into());
        }
        let process_100ns = filetime_to_100ns(kernel) + filetime_to_100ns(user);

        let mut idle = creation;
        let mut system_kernel = creation;
        let mut system_user = creation;
        // SAFETY: three valid out-params, no other preconditions.
        let ok = unsafe { GetSystemTimes(&mut idle, &mut system_kernel, &mut system_user) };
        if ok == 0 {
            return Err("GetSystemTimes failed".into());
        }
        // Windows folds idle time into the kernel figure, so busy time is
        // kernel + user - idle.
        let system_busy_100ns = (filetime_to_100ns(system_kernel) + filetime_to_100ns(system_user))
            .saturating_sub(filetime_to_100ns(idle));

        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: 0,
            PageFaultCount: 0,
            PeakWorkingSetSize: 0,
            WorkingSetSize: 0,
            QuotaPeakPagedPoolUsage: 0,
            QuotaPagedPoolUsage: 0,
            QuotaPeakNonPagedPoolUsage: 0,
            QuotaNonPagedPoolUsage: 0,
            PagefileUsage: 0,
            PeakPagefileUsage: 0,
            PrivateUsage: 0,
        };
        let size = u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS_EX>()).unwrap_or(0);
        counters.cb = size;
        // SAFETY: PROCESS_MEMORY_COUNTERS_EX is the documented extended form of
        // PROCESS_MEMORY_COUNTERS and `cb` tells the API which it received.
        let ok = unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                std::ptr::from_mut(&mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
                size,
            )
        };
        if ok == 0 {
            return Err("GetProcessMemoryInfo failed".into());
        }

        Ok(CostSample {
            at: Instant::now(),
            process_100ns,
            system_busy_100ns,
            working_set: counters.WorkingSetSize,
            private: counters.PrivateUsage,
        })
    }

    /// The cost of one window, as percentages of a single core.
    struct CostWindow {
        seconds: f64,
        process_percent: f64,
        /// The raw CPU time behind `process_percent`.
        ///
        /// **Reported because the percentage alone cannot be trusted at these
        /// magnitudes.** `GetProcessTimes` accounts in scheduler ticks
        /// (~15.6 ms), so a genuinely tiny cost and a cost below the clock's
        /// resolution both print as `0.00 %`. Showing the milliseconds makes
        /// "0 ms of 8000 ms" legible as *below what this instrument can see*
        /// rather than as a measured zero — which is the `UT-F-41` /
        /// `I-11` failure this project keeps finding.
        process_ms: f64,
        system_percent: f64,
        working_set: usize,
        private: usize,
    }

    fn window_between(start: CostSample, end: CostSample) -> CostWindow {
        let seconds = end.at.duration_since(start.at).as_secs_f64().max(1e-9);
        // 100 ns units per second of one core.
        let per_core = seconds * 10_000_000.0;
        let process_100ns = end.process_100ns.saturating_sub(start.process_100ns);
        CostWindow {
            seconds,
            process_percent: (process_100ns as f64) / per_core * 100.0,
            process_ms: process_100ns as f64 / 10_000.0,
            system_percent: (end
                .system_busy_100ns
                .saturating_sub(start.system_busy_100ns) as f64)
                / per_core
                * 100.0,
            working_set: end.working_set,
            private: end.private,
        }
    }

    let mib = |bytes: usize| bytes as f64 / (1024.0 * 1024.0);
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;

    // ---------------------------------------------------------------------
    // Setup
    // ---------------------------------------------------------------------

    // SAFETY: no memory-safety preconditions; must run before any HWND exists.
    let aware =
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if aware == 0 {
        return Err("could not enable per-monitor DPI awareness".into());
    }

    let mut seconds = 8u64;
    let mut shots = 5usize;
    let mut cold = false;
    let mut limit: Option<usize> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cold" => cold = true,
            "--seconds" => seconds = args.next().unwrap_or_default().parse()?,
            "--shots" => shots = args.next().unwrap_or_default().parse()?,
            "--monitors" => limit = Some(args.next().unwrap_or_default().parse()?),
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let window = Duration::from_secs(seconds);

    let cores = std::thread::available_parallelism().map_or(1.0, |n| n.get() as f64);
    let mut monitors = Monitor::enumerate()?;
    if monitors.is_empty() {
        return Err("no monitors enumerated".into());
    }
    // `--monitors N` holds the first N enumerated rather than all of them.
    //
    // Added 2026-08-08 because ADR-0026's third amendment holds the warm-path
    // default flip behind a measurement of the NARROWED configuration, taken
    // "with the same instrument and the same two conditions" as the +0.62 /
    // +0.94 pp figures — and this program had no way to hold fewer than every
    // monitor, so that release condition was unexecutable from the day it was
    // written. `UT-F-50` is this project's record of an owed rig check that
    // reads like work nobody got round to and could not have been performed.
    //
    // **The first N, not the cursor's, and the difference is stated rather than
    // hidden.** The app narrows to the monitor under the pointer, which moves; a
    // fixed subset is what makes two runs comparable, which is this program's
    // whole job. So a run here is not a simulation of the app's behaviour, it is
    // a measurement of what holding N sessions costs.
    //
    // The startup line names the exact monitors held. A subset that did not say
    // which subset would be `UT-F-46` and `UT-F-56` in one: a number nobody can
    // attribute to a configuration.
    if let Some(count) = limit {
        if count == 0 || count > monitors.len() {
            return Err(format!(
                "--monitors {count} is out of range: {} monitor(s) enumerated",
                monitors.len()
            )
            .into());
        }
        monitors.truncate(count);
    }

    println!(
        "warm_session — {} mode, {} monitor(s){}, {seconds}s cost window, {shots} shot(s), \
         {cores:.0} logical cores",
        if cold { "COLD (control)" } else { "WARM" },
        monitors.len(),
        match limit {
            Some(count) => format!(" (--monitors {count}, the first {count} enumerated)"),
            None => " (every monitor)".to_string(),
        },
    );
    println!(
        "  Both CPU figures are percent of ONE core, matching quality-bars.md §1 — so \
         on this machine the system figure can reach {:.0} % before every core is busy. \
         It covers the whole machine (dwm.exe included) and is noisy by nature: read it \
         as an upper bound on our cost, never as our cost.",
        cores * 100.0,
    );
    println!(
        "  RAM is reported twice on purpose. Working set is resident pages; private is \
         committed bytes, and D3D11 commits far more than it resides. §1's 80 MB idle-RAM \
         row does not say which it means — decide that before quoting either against it."
    );
    println!();

    // ---------------------------------------------------------------------
    // Phase 0 — the baseline: this process, holding nothing
    // ---------------------------------------------------------------------

    println!("[0/3] baseline — no sessions held, {seconds}s. Leave the machine alone.");
    let baseline_start = sample_cost()?;
    std::thread::sleep(window);
    let baseline = window_between(baseline_start, sample_cost()?);
    println!(
        "      over {:.1}s: ours {:.2} % ({:.1} ms of CPU)  system {:.1} %  \
         working set {:.1} MiB  private {:.1} MiB",
        baseline.seconds,
        baseline.process_percent,
        baseline.process_ms,
        baseline.system_percent,
        mib(baseline.working_set),
        mib(baseline.private),
    );
    println!();

    // ---------------------------------------------------------------------
    // Phase 1 — hold the sessions (warm mode only)
    // ---------------------------------------------------------------------

    let mut slots: Vec<Arc<Slot>> = Vec::new();
    let live_sessions = Arc::new(AtomicUsize::new(0));

    if cold {
        println!("[1/3] cold control — no sessions started, nothing retained.");
        println!();
    } else {
        for (index, monitor) in monitors.iter().enumerate() {
            let label = monitor
                .name()
                .unwrap_or_else(|_| format!("monitor {}", index + 1));
            slots.push(Arc::new(Slot {
                label,
                handle: monitor.as_raw_hmonitor() as isize,
                retained: Mutex::new(None),
                arrivals: Mutex::new(Vec::new()),
                stop: AtomicBool::new(false),
                thread_id: Mutex::new(None),
                failure: Mutex::new(None),
            }));
        }

        println!("[1/3] starting {} session(s)…", slots.len());
        let started = Instant::now();
        for slot in &slots {
            let slot = Arc::clone(slot);
            let live = Arc::clone(&live_sessions);
            live.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || {
                // Create the message queue before reporting the thread id —
                // PostThreadMessageW fails against a thread that has none yet.
                // Same preamble as `wgc.rs`, and for the same reason: a session
                // on a static desktop may never get another frame, so `stop`
                // alone cannot unwind it.
                // SAFETY: PeekMessageW with PM_NOREMOVE only inspects the queue
                // (creating it as a side effect); MSG is a plain struct.
                unsafe {
                    let mut msg: MSG = std::mem::zeroed();
                    PeekMessageW(
                        &mut msg,
                        std::ptr::null_mut(),
                        WM_USER,
                        WM_USER,
                        PM_NOREMOVE,
                    );
                }
                *slot
                    .thread_id
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) =
                    // SAFETY: no preconditions.
                    Some(unsafe { GetCurrentThreadId() });

                let monitor = Monitor::from_raw_hmonitor(slot.handle as *mut std::ffi::c_void);
                let settings = Settings::new(
                    monitor,
                    CursorCaptureSettings::WithoutCursor,
                    DrawBorderSettings::WithoutBorder,
                    SecondaryWindowSettings::Default,
                    MinimumUpdateIntervalSettings::Default,
                    DirtyRegionSettings::Default,
                    ColorFormat::Rgba8,
                    WarmFlags {
                        slot: Arc::clone(&slot),
                    },
                );
                // `start` blocks pumping until the handler stops the session or
                // WM_QUIT unwinds the loop, so returning from it *is* the
                // session being over — which is why the count is decremented
                // here and not in `on_closed`.
                if let Err(error) = Warm::start(settings) {
                    slot.note_failure(format!("the session could not start: {error}"));
                }
                live.fetch_sub(1, Ordering::SeqCst);
            });
        }

        // Wait for every session's first frame. WGC delivers one promptly after
        // a session starts (~90 ms on this rig) — it is *subsequent* frames
        // that are event-driven — so a slot with nothing here is a real
        // failure, not a still screen.
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && slots.iter().any(|s| s.arrival_times().is_empty()) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let missing = slots
            .iter()
            .filter(|s| s.arrival_times().is_empty())
            .count();
        println!(
            "      first frame on every session after {:.0} ms{}",
            ms(started.elapsed()),
            if missing == 0 {
                String::new()
            } else {
                format!(" — except {missing}, which delivered nothing and are NOT warm")
            },
        );
        println!();
    }

    // ---------------------------------------------------------------------
    // Phase 2 — the cost of holding them
    // ---------------------------------------------------------------------

    println!("[2/3] cost window — {seconds}s with the sessions held. Leave the machine alone.");
    let held_start = sample_cost()?;
    let held_window_started = Instant::now();
    std::thread::sleep(window);
    let held = window_between(held_start, sample_cost()?);
    let held_window_ended = Instant::now();

    println!(
        "      over {:.1}s: ours {:.2} % ({:.1} ms of CPU)  system {:.1} %  \
         working set {:.1} MiB  private {:.1} MiB",
        held.seconds,
        held.process_percent,
        held.process_ms,
        held.system_percent,
        mib(held.working_set),
        mib(held.private),
    );
    println!(
        "      DELTA vs baseline: ours {:+.2} pp   system {:+.1} pp   private {:+.1} MiB",
        held.process_percent - baseline.process_percent,
        held.system_percent - baseline.system_percent,
        mib(held.private) - mib(baseline.private),
    );
    println!();

    // ---------------------------------------------------------------------
    // The armed signal — is each session actually live and holding pixels?
    // ---------------------------------------------------------------------

    let mut retained_bytes = 0usize;
    if !cold {
        println!("      sessions, at the end of the cost window:");
        for slot in &slots {
            let at = slot.arrival_times();
            let in_window = at
                .iter()
                .filter(|t| **t >= held_window_started && **t <= held_window_ended)
                .count();
            let failure = slot
                .failure
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            let dims = {
                let guard = slot.retained.lock().unwrap_or_else(PoisonError::into_inner);
                guard.as_ref().map(|r| (r.width, r.height))
            };
            match (dims, failure) {
                (_, Some(reason)) => {
                    println!("        {} — NOT WARM: {reason}", slot.label);
                }
                (None, None) => {
                    println!(
                        "        {} — NOT WARM: no frame ever arrived, nothing retained",
                        slot.label
                    );
                }
                (Some((w, h)), None) => {
                    // live + staging: the GPU copy and the CPU-readable one.
                    let bytes = w as usize * h as usize * 4;
                    retained_bytes += bytes * 2;
                    let age = at
                        .last()
                        .map(|t| ms(held_window_ended.duration_since(*t)))
                        .unwrap_or_default();
                    let gap = at
                        .windows(2)
                        .map(|w| w[1] - w[0])
                        .chain(
                            at.last()
                                .map(|l| held_window_ended.saturating_duration_since(*l)),
                        )
                        .max()
                        .unwrap_or_default();
                    println!(
                        "        {} — warm, {w}×{h}, {in_window} frame(s) in the window, \
                         newest {age:.0} ms old, longest silence {:.0} ms",
                        slot.label,
                        ms(gap),
                    );
                }
            }
        }
        println!(
            "      retained by construction: {:.1} MiB across {} session(s) \
             — half VRAM (the live texture), half CPU-readable (the staging one)",
            mib(retained_bytes),
            slots.len(),
        );
        println!();
    }

    // ---------------------------------------------------------------------
    // Phase 3 — the shots: what a Ctrl+Space costs
    // ---------------------------------------------------------------------

    let desktop = {
        // SAFETY: GetSystemMetrics takes an index and returns an int.
        let (x, y, w, h) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        uptake_core::geometry::Rect::new(
            x,
            y,
            u32::try_from(w).unwrap_or(0),
            u32::try_from(h).unwrap_or(0),
        )
    };

    println!("[3/3] {shots} shot(s) — each one is a simulated Ctrl+Space.");
    let mut latencies: Vec<Duration> = Vec::new();
    let mut ages: Vec<Duration> = Vec::new();
    let mut failures = 0usize;

    for shot in 1..=shots {
        std::thread::sleep(Duration::from_millis(900));
        let pressed = Instant::now();

        if cold {
            match uptake_capture::capture_region(desktop) {
                Ok(captured) => {
                    let elapsed = pressed.elapsed();
                    latencies.push(elapsed);
                    println!(
                        "      shot {shot}: {:.0} ms — fresh capture of {}×{}",
                        ms(elapsed),
                        captured.rect.size.width,
                        captured.rect.size.height,
                    );
                }
                Err(error) => {
                    failures += 1;
                    println!("      shot {shot}: FAILED — {error}");
                }
            }
            continue;
        }

        // The age of what we are about to hand over, read at the keypress —
        // an input to the fidelity question, never an answer to it.
        let oldest = slots
            .iter()
            .filter_map(|s| s.arrival_times().last().copied())
            .map(|t| pressed.saturating_duration_since(t))
            .max();

        let mut bytes = 0usize;
        let mut failed: Option<String> = None;
        for slot in &slots {
            match readback(slot) {
                Ok(n) => bytes += n,
                Err(reason) => {
                    failed = Some(format!("{}: {reason}", slot.label));
                    break;
                }
            }
        }
        let elapsed = pressed.elapsed();

        match failed {
            Some(reason) => {
                failures += 1;
                println!("      shot {shot}: FAILED — {reason}");
            }
            None => {
                latencies.push(elapsed);
                if let Some(age) = oldest {
                    ages.push(age);
                }
                println!(
                    "      shot {shot}: {:.0} ms — {:.1} MiB read back from {} session(s); \
                     oldest held frame {:.0} ms",
                    ms(elapsed),
                    mib(bytes),
                    slots.len(),
                    oldest.map_or(f64::NAN, ms),
                );
            }
        }
    }

    // ---------------------------------------------------------------------
    // Teardown, then the verdict
    // ---------------------------------------------------------------------

    if !cold {
        let stopping = Instant::now();
        for slot in &slots {
            slot.stop.store(true, Ordering::SeqCst);
            if let Some(id) = *slot
                .thread_id
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
            {
                // SAFETY: posting a thread message has no memory-safety
                // preconditions. The pump threads are still alive here — none
                // of them can exit before its session stops, and only this
                // post or a frame can stop one.
                unsafe {
                    PostThreadMessageW(id, WM_QUIT, 0, 0);
                }
            }
        }
        let deadline = stopping + Duration::from_secs(2);
        while live_sessions.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        println!();
        println!(
            "      teardown: {} session(s) still live after {:.0} ms",
            live_sessions.load(Ordering::SeqCst),
            ms(stopping.elapsed()),
        );
    }

    println!();
    println!("--- result ---");
    if latencies.is_empty() {
        println!(
            "NO SHOT SUCCEEDED ({failures} failed). This is a null result, not a fast one — \
             do not read the cost window as the cost of a working warm path."
        );
        return Ok(());
    }
    let total: Duration = latencies.iter().sum();
    let mean = total / u32::try_from(latencies.len()).unwrap_or(1);
    let worst = latencies.iter().max().copied().unwrap_or_default();
    println!(
        "shot latency: mean {:.0} ms, worst {:.0} ms over {} shot(s) ({failures} failed)",
        ms(mean),
        ms(worst),
        latencies.len(),
    );
    if !ages.is_empty() {
        let oldest = ages.iter().max().copied().unwrap_or_default();
        println!(
            "held-frame age at the keypress: worst {:.0} ms — an INPUT to §1's fidelity row, \
             not a reading of it (see that row's footnote)",
            ms(oldest),
        );
    }
    println!(
        "cost of holding: ours {:+.2} pp, system {:+.1} pp, private {:+.1} MiB against baseline",
        held.process_percent - baseline.process_percent,
        held.system_percent - baseline.system_percent,
        mib(held.private) - mib(baseline.private),
    );
    println!();
    println!("Reading it:");
    println!(
        "  §1 bounds `CPU — overlay visible, passive only` at < 1.5 % of one core, of which \
         the click-through poll already spends 0.63 %. The warm sessions must fit in what is left."
    );
    println!(
        "  If holding costs ~nothing on a static desktop, 1.9f's setting is complexity nobody \
         needed and the warm path should be the default (ADR-0026's amendment says so in those words)."
    );
    println!(
        "  If it only costs with video playing, the honest options are a setting or throttling \
         via MinimumUpdateIntervalSettings — which this program deliberately leaves at Default, \
         the same value wgc.rs captures with."
    );
    println!(
        "  Compare warm against `--cold` before believing any of it: if the shot latencies match, \
         nothing was retained and the run is void."
    );
    println!("  Two ways this run can fail to measure anything, both of which look like a result:");
    println!(
        "    - `ours` printing 0.00 % with ~0 ms of CPU beside it means the cost is below \
         GetProcessTimes' ~15.6 ms tick, NOT that it is zero. Lengthen --seconds until the \
         millisecond figure moves, or say 'under the resolution of the instrument' and mean it."
    );
    println!(
        "    - the `system` delta is the whole machine. Read the SAME delta off a `--cold` run: \
         cold holds nothing, so whatever it reports there is noise, and if that is the size of \
         the warm delta then this channel resolved nothing and dwm's share is still unknown."
    );
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("warm_session only runs on Windows — uptake-capture is the Windows capture surface");
}
