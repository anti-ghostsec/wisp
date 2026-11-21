//! Timing runs for testing how fast Wisp is (`WISP_PERF_SCRIPT=file`).
//!
//! Off unless that variable is set. The file has one command per line:
//!
//!   load URL           load URL in the current tab, wait for it to finish
//!   tab URL            the same in a new tab
//!   settle SECS        wait (up to SECS) until the page's thread has had
//!                      no task over 50 ms for 2 s; prints when that began
//!   click SELECTOR     click the page's element (and restart the clock)
//!   waitfor SECS JS    wait until the page's JS expression is true
//!   js JS              print the page's JS expression
//!   fps SECS           scroll the page down for SECS, frame by frame;
//!                      prints frames per second and the longest frame
//!   window URL         open URL in a new window (later commands use it)
//!   closewin [first]   close the newest (or the first) window
//!   closetab           close the current tab
//!   views              how many pages are still alive, and playing sound
//!   vault PASSWORD     set up (or unlock) a test vault with that password
//!   sync URL           set up a sync account on that (test) service
//!   bookmarks N        add N test bookmarks
//!   rss                Wisp's own memory in use
//!   bar                how many items the bookmarks bar shows
//!   tabs               the tabs (* marks the current one)
//!   closebutton N      where tab N's close button is in the window
//!   popup PLUGIN/ACTION  press a plugin's toolbar button; while its pop-up
//!                      is open, commands go to it
//!   sleep SECS
//!   quit
//!
//! Each prints a `perf:` line: times in ms since the command began, the
//! page's own navigation timing, and how long the page's thread and Wisp's
//! window thread were blocked (tasks over 50 ms: longest, total).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};
use webkit6::prelude::*;
use webkit6::{glib, LoadEvent, WebView};

/// The page's probe runs in a script world of its own.
pub const WORLD: &str = "wisp-perf";

/// Measures how often the page's thread is blocked (as long tasks do),
/// from the start of the page.
pub const PROBE: &str = r#"(() => {
  const s = globalThis.__wispPerf = { max: 0, blocked: 0, longs: 0, lastLong: 0 };
  let last = performance.now();
  const tick = () => {
    const now = performance.now(), gap = now - last;
    if (gap > 50) { s.blocked += gap - 50; s.longs++; s.lastLong = now; if (gap > s.max) s.max = gap; }
    last = now;
    setTimeout(tick, 16);
  };
  setTimeout(tick, 16);
})();"#;

const REPORT: &str = r#"JSON.stringify((() => {
  const n = performance.getEntriesByType("navigation")[0] || {};
  const s = globalThis.__wispPerf || {};
  const r = (x) => Math.round(x || 0);
  return { dcl: r(n.domContentLoadedEventEnd), load: r(n.loadEventEnd), resources: performance.getEntriesByType("resource").length,
           pageMax: r(s.max), pageBlocked: r(s.blocked), pageLongs: s.longs || 0, lastLong: r(s.lastLong), now: r(performance.now()) };
})())"#;

thread_local! {
    static FINISHED: Cell<u64> = const { Cell::new(0) };
    static COMMITTED: Cell<Option<Instant>> = const { Cell::new(None) };
    static UI: RefCell<(Instant, f64, f64)> = RefCell::new((Instant::now(), 0.0, 0.0));
}

pub fn enabled() -> bool {
    std::env::var_os("WISP_PERF_SCRIPT").is_some()
}

/// Called on every main-frame load event of every view.
pub fn load_event(event: LoadEvent) {
    match event {
        LoadEvent::Committed => COMMITTED.with(|c| c.set(Some(Instant::now()))),
        LoadEvent::Finished => FINISHED.with(|f| f.set(f.get() + 1)),
        _ => {}
    }
}

fn ui_reset() {
    UI.with(|u| *u.borrow_mut() = (Instant::now(), 0.0, 0.0));
}

fn ui_lag() -> (f64, f64) {
    UI.with(|u| {
        let u = u.borrow();
        (u.1, u.2)
    })
}

async fn eval(view: &WebView, js: &str, world: Option<&str>) -> String {
    match view.evaluate_javascript_future(js, world, None).await {
        Ok(v) => v.to_str().to_string(),
        Err(e) => format!("error: {e}"),
    }
}

/// Run the script in `file` against the window's current tab.
pub fn run(
    file: String,
    view: impl Fn() -> Option<WebView> + 'static,
    go: impl Fn(&str, bool) + 'static,
    quit: impl Fn() + 'static,
    other: impl Fn(&str, &str) -> Option<String> + 'static,
) {
    // Wisp's window thread: tasks over 50 ms.
    glib::timeout_add_local(Duration::from_millis(16), || {
        UI.with(|u| {
            let mut u = u.borrow_mut();
            let gap = u.0.elapsed().as_secs_f64() * 1000.0;
            if gap > 50.0 {
                u.2 += gap - 50.0;
                u.1 = f64::max(u.1, gap);
            }
            u.0 = Instant::now();
        });
        glib::ControlFlow::Continue
    });
    let view = Rc::new(view);
    glib::spawn_future_local(async move {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        glib::timeout_future(Duration::from_secs(2)).await;
        let mut mark = Instant::now();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
            let Some(v) = view() else { break };
            let ms = |t: Instant| t.elapsed().as_millis();
            match cmd {
                "load" | "tab" => {
                    let before = FINISHED.with(|f| f.get());
                    COMMITTED.with(|c| c.set(None));
                    ui_reset();
                    let t0 = Instant::now();
                    go(arg, cmd == "tab");
                    glib::timeout_future(Duration::from_millis(50)).await;
                    let Some(v) = view() else { break };
                    while FINISHED.with(|f| f.get()) == before && t0.elapsed() < Duration::from_secs(90) {
                        glib::timeout_future(Duration::from_millis(10)).await;
                    }
                    let finished = ms(t0);
                    let committed = COMMITTED.with(|c| c.get()).map(|c| c.duration_since(t0).as_millis());
                    let (ui_max, ui_blocked) = ui_lag();
                    let page = eval(&v, REPORT, Some(WORLD)).await;
                    println!("perf: {cmd} {arg} committed={committed:?} finished={finished} ui_max={ui_max:.0} ui_blocked={ui_blocked:.0} page={page}");
                    mark = t0;
                }
                "settle" => {
                    let limit = Duration::from_secs_f64(arg.parse().unwrap_or(20.0));
                    let t0 = Instant::now();
                    let mut page = String::new();
                    while t0.elapsed() < limit {
                        page = eval(&v, REPORT, Some(WORLD)).await;
                        let j: serde_json::Value = serde_json::from_str(&page).unwrap_or_default();
                        if j["now"].as_f64().unwrap_or(0.0) - j["lastLong"].as_f64().unwrap_or(0.0) > 2000.0 {
                            break;
                        }
                        glib::timeout_future(Duration::from_millis(200)).await;
                    }
                    let (ui_max, ui_blocked) = ui_lag();
                    println!("perf: settle since_load={} ui_max={ui_max:.0} ui_blocked={ui_blocked:.0} page={page}", ms(mark));
                }
                "click" => {
                    ui_reset();
                    let js = format!("(() => {{ const e = document.querySelector({arg:?}); if (!e) return 'missing'; e.click(); return 'ok'; }})()");
                    mark = Instant::now();
                    let r = eval(&v, &js, None).await;
                    println!("perf: click {arg} {r}");
                }
                "waitfor" => {
                    let (secs, expr) = arg.split_once(' ').unwrap_or(("30", arg));
                    let limit = Duration::from_secs_f64(secs.parse().unwrap_or(30.0));
                    let t0 = Instant::now();
                    let mut ok = false;
                    while t0.elapsed() < limit {
                        if eval(&v, &format!("String(!!({expr}))"), None).await == "true" {
                            ok = true;
                            break;
                        }
                        glib::timeout_future(Duration::from_millis(50)).await;
                    }
                    let (ui_max, ui_blocked) = ui_lag();
                    println!("perf: waitfor {} after={} ui_max={ui_max:.0} ui_blocked={ui_blocked:.0} :: {expr}", if ok { "ok" } else { "TIMEOUT" }, ms(mark));
                }
                "fps" => {
                    let secs: f64 = arg.parse().unwrap_or(3.0);
                    let body = format!(r#"return await new Promise((done) => {{
                        const end = performance.now() + {ms}; let n = 0, last = performance.now(), worst = 0;
                        const step = (t) => {{ n++; worst = Math.max(worst, t - last); last = t; scrollBy(0, 30);
                            if (t < end) requestAnimationFrame(step); else done(JSON.stringify({{ fps: Math.round(n / {secs}), worst: Math.round(worst) }})); }};
                        requestAnimationFrame(step); }});"#, ms = secs * 1000.0);
                    ui_reset();
                    let r = match v.call_async_javascript_function_future(&body, None, None, None).await {
                        Ok(r) => r.to_str().to_string(),
                        Err(e) => format!("error: {e}"),
                    };
                    let (ui_max, ui_blocked) = ui_lag();
                    println!("perf: fps {r} ui_max={ui_max:.0} ui_blocked={ui_blocked:.0}");
                }
                "js" => println!("perf: js {} :: {arg}", eval(&v, arg, None).await),
                "sleep" => glib::timeout_future(Duration::from_secs_f64(arg.parse().unwrap_or(1.0))).await,
                "quit" => break,
                _ => match other(cmd, arg) {
                    Some(out) => println!("perf: {cmd} {out}"),
                    None => eprintln!("perf: unknown command {cmd}"),
                },
            }
        }
        quit();
    });
}
