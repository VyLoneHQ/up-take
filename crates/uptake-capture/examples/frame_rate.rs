//! Counts WGC `FrameArrived` callbacks per second, to settle whether a warm
//! capture session costs anything while the screen is static.
//!
//! # The claim this exists to test
//!
//! [ADR-0022] rejects a **persistent capture service** — WGC sessions held warm
//! per monitor — as the way to meet the selection→clipboard budget, and the
//! argument turns on one premise: WGC is `FrameArrived`-driven, so a warm
//! session has nothing to hand over until the compositor produces a frame, and
//! to answer "give me pixels now" it must therefore retain a full-monitor copy
//! of every frame delivered. That is ~33 MB per monitor and a continuous copy on
//! any moving content, against `quality-bars.md` §1's two tightest rows.
//!
//! The premise is **documented behaviour and inference, not something observed
//! on this rig** — and the ADR says so in its own words, in a section headed
//! "An assumption that is inference, not verification". This project's record
//! (F-15, F-25, F-27, F-32, F-33, and the unrun equivalence argument of
//! 2026-07-27 that broke the Start menu) is that Win32/WinRT inference is
//! precisely where its defects live. So the experiment is owed rather than
//! assumed, and ADR-0022's *Revisit if* names the result that would overturn it:
//! **frames arriving continuously on a static desktop**.
//!
//! # Running it
//!
//! ```text
//! cargo run --release -p uptake-capture --example frame_rate -- [seconds]
//! ```
//!
//! **Captures the monitor the cursor is on**, so which screen is measured is
//! chosen by parking the mouse there — which matters, because the monitor
//! running the terminal that launched this is by definition not static.
//!
//! Two runs, and the comparison between them is the answer:
//!
//! 1. **Static desktop** — nothing moving, no cursor over an animating surface,
//!    no blinking caret in view. This is the number the ADR rests on.
//! 2. **Video playing** on the same monitor, as a positive control: it proves
//!    the counter and the session work at all, so that a low static reading is
//!    "the compositor produced nothing" and not "this program is broken". A
//!    static reading with no control run beside it cannot tell those apart, and
//!    reporting one alone would be the F-17/F-33 shape — a check that looks like
//!    evidence while measuring nothing.
//!
//! Uses `MinimumUpdateIntervalSettings::Default`, the same setting `wgc.rs`
//! captures with. Throttling it would answer a different question than the one
//! the ADR asked.
//!
//! [ADR-0022]: the private planning repo's
//! `DECISIONS/ADR-0022-hold-a-frame-and-crop.md`

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
    use windows_capture::frame::Frame;
    use windows_capture::graphics_capture_api::InternalCaptureControl;
    use windows_capture::monitor::Monitor;
    use windows_capture::settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    };
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };

    /// Every frame's arrival time, which is the measurement.
    ///
    /// # Why timestamps and not just a count
    ///
    /// A rate alone cannot answer the question. "86 frames in 5 s" is equally
    /// consistent with the compositor delivering **continuously at 16 fps** —
    /// which would overturn ADR-0022 point 7, since a warm session would then be
    /// paying for a full-monitor frame six times a second forever — and with it
    /// delivering **one short burst** when something on screen changed and
    /// nothing at all either side of it, which is exactly what point 7 claims.
    /// The average is identical; the systems are opposites.
    ///
    /// So the statistic that decides it is the **longest silent gap**: a
    /// genuinely event-driven session on a still screen has long gaps, and a
    /// continuous one has none longer than a frame interval. Recording arrival
    /// times and deriving the gaps afterwards keeps the handler to a `push`.
    #[derive(Default)]
    struct Arrivals {
        at: Vec<Instant>,
    }

    /// What the counting handler needs: somewhere to record, and when to stop.
    struct CountFlags {
        frames: Arc<AtomicU64>,
        arrivals: Arc<std::sync::Mutex<Arrivals>>,
        until: Instant,
    }

    struct Count {
        flags: CountFlags,
    }

    impl GraphicsCaptureApiHandler for Count {
        type Flags = CountFlags;
        type Error = String;

        fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
            Ok(Self { flags: ctx.flags })
        }

        fn on_frame_arrived(
            &mut self,
            _frame: &mut Frame<'_>,
            capture_control: InternalCaptureControl,
        ) -> Result<(), Self::Error> {
            // Deliberately does nothing with the pixels. Copying them out would
            // add the very cost the ADR is arguing about to the measurement of
            // how often it would be paid.
            self.flags.frames.fetch_add(1, Ordering::Relaxed);
            let now = Instant::now();
            self.flags
                .arrivals
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .at
                .push(now);
            if now >= self.flags.until {
                capture_control.stop();
            }
            Ok(())
        }

        fn on_closed(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    // SAFETY: no memory-safety preconditions; must run before any HWND exists.
    let aware =
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if aware == 0 {
        return Err("could not enable per-monitor DPI awareness".into());
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let seconds: u64 = args.first().map_or(Ok(5), |s| s.parse())?;
    let duration = Duration::from_secs(seconds);

    // The monitor under the cursor, as a raw `HMONITOR`. Resolved here and
    // carried into the pump thread as an `isize` for the same reason `wgc.rs`
    // does it: a `Monitor` wraps a raw pointer and is not `Send`, so it is
    // rebuilt on the thread that uses it.
    // SAFETY: both calls take an out-param / plain value and have no
    // memory-safety preconditions; a null cursor position is not possible here.
    let handle = unsafe {
        let mut point = std::mem::zeroed();
        if windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point) == 0 {
            return Err("could not read the cursor position".into());
        }
        windows_sys::Win32::Graphics::Gdi::MonitorFromPoint(
            point,
            windows_sys::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST,
        ) as isize
    };

    let frames = Arc::new(AtomicU64::new(0));
    let arrivals = Arc::new(std::sync::Mutex::new(Arrivals::default()));
    let started = Instant::now();
    let until = started + duration;

    println!(
        "counting FrameArrived on the monitor under the cursor for {seconds}s \
         — leave that screen alone"
    );

    {
        let frames = Arc::clone(&frames);
        let arrivals = Arc::clone(&arrivals);
        std::thread::spawn(move || {
            let monitor = Monitor::from_raw_hmonitor(handle as *mut std::ffi::c_void);
            let settings = Settings::new(
                monitor,
                CursorCaptureSettings::WithoutCursor,
                DrawBorderSettings::WithoutBorder,
                SecondaryWindowSettings::Default,
                MinimumUpdateIntervalSettings::Default,
                DirtyRegionSettings::Default,
                ColorFormat::Rgba8,
                CountFlags {
                    frames,
                    arrivals,
                    until,
                },
            );
            // Blocks pumping messages until the handler stops the session — and
            // on a genuinely static desktop the handler may **never run**, so
            // this thread may never return. That is not a hang to be fixed, it
            // is the result being measured: a session that delivers no frame
            // cannot answer a capture request on demand, which is ADR-0022
            // point 7's whole argument. The main thread therefore never joins
            // this one; it waits on the clock and reports.
            if let Err(error) = Count::start(settings) {
                eprintln!("frame_rate: the capture session failed: {error}");
            }
        });
    }

    // Waits on the wall clock, not on the pump. A little longer than the
    // sample window so a session that *is* delivering has time to stop itself
    // and be counted, rather than being cut off mid-window.
    std::thread::sleep(duration + Duration::from_millis(250));
    let elapsed = started.elapsed();

    let count = frames.load(Ordering::Relaxed);
    let at = std::mem::take(
        &mut arrivals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .at,
    );
    let rate = f64::from(u32::try_from(count).unwrap_or(u32::MAX))
        / elapsed.as_secs_f64().max(f64::EPSILON);
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;

    let Some(&first) = at.first() else {
        println!(
            "no frame arrived in {:.2}s — the session delivered nothing",
            elapsed.as_secs_f64()
        );
        println!(
            "ADR-0022 point 7 HOLDS on this evidence: a warm session has nothing \
             to hand over until the compositor produces a frame."
        );
        return Ok(());
    };

    // The gap that decides it. Measured from the first frame onward, so the
    // session-setup wait (~90–340 ms before any frame can arrive) is not counted
    // as compositor silence — that would manufacture a long gap on every run,
    // including a continuously-delivering one, and the statistic would then
    // confirm ADR-0022 no matter what the compositor did.
    let longest_gap = at.windows(2).map(|w| w[1] - w[0]).max().unwrap_or_default();
    let one_second_or_more = at
        .windows(2)
        .filter(|w| w[1] - w[0] >= Duration::from_secs(1))
        .count();
    let sampled = at.last().map_or(Duration::ZERO, |last| *last - first);

    println!(
        "{count} frames in {:.2}s = {rate:.1} fps (first frame after {:.1} ms)",
        elapsed.as_secs_f64(),
        ms(first - started),
    );
    println!(
        "longest silent gap after the first frame: {:.0} ms over a {:.2}s window \
         ({one_second_or_more} gaps of 1s or more)",
        ms(longest_gap),
        sampled.as_secs_f64(),
    );
    println!();
    println!("Reading it — the gap decides, not the rate:");
    println!(
        "  long gaps  -> event-driven. ADR-0022 point 7 holds: a warm session \
         idles for free but has nothing to hand over on demand."
    );
    println!(
        "  no gaps    -> continuous delivery. Point 7's cost argument is \
         OVERTURNED and the ADR says to revisit it on exactly this evidence."
    );
    println!(
        "  Run it twice — still screen, then with video on that monitor. The \
         video run is the control that proves the counter works at all."
    );
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("frame_rate only runs on Windows — uptake-capture is the Windows capture surface");
}
