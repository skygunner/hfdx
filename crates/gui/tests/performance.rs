#[path = "../src/bridge.rs"]
mod bridge;
#[path = "../src/callbacks.rs"]
mod callbacks;
#[path = "../src/files.rs"]
mod files;
#[path = "../src/input.rs"]
mod input;
#[path = "../src/queue_view.rs"]
mod queue_view;
mod support;
#[path = "../src/view.rs"]
mod view;
slint::include_modules!();

use hfdx_core::{
    storage::SettingsPaths,
    task::{CoreConfig, UiPhase},
};
use slint::{ComponentHandle, Model};
use std::{
    cell::RefCell,
    io::Write,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Measurement {
    index: usize,
    revision: u64,
    started: Instant,
    csv: std::fs::File,
}

fn resources() -> (u64, u64, u64) {
    let status = std::fs::read_to_string("/proc/self/status").expect("Linux process status");
    let value = |key| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .and_then(|s| s.split_whitespace().next())
            .expect("process memory field")
            .parse::<u64>()
            .expect("memory KiB")
    };
    let stat = std::fs::read_to_string("/proc/self/stat").expect("process CPU stat");
    let fields: Vec<_> = stat
        .rsplit_once(')')
        .expect("process name")
        .1
        .split_whitespace()
        .collect();
    let ticks = fields[11].parse::<u64>().expect("user ticks")
        + fields[12].parse::<u64>().expect("system ticks");
    (value("VmRSS:"), value("VmHWM:"), ticks)
}

#[test]
fn native_catalog_callback_to_render_measurements() {
    let entered = Instant::now();
    let entry_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("epoch")
        .as_nanos();
    let launch_override = std::env::var("HFDX_GUI_LAUNCH_NS").ok();
    let launch_ns = launch_override.as_ref().map_or(entry_ns, |value| {
        value.parse::<u128>().expect("pre-spawn epoch nanoseconds")
    });
    let count = std::env::var("HFDX_GUI_CATALOG_COUNT")
        .map(|value| value.parse::<usize>().expect("catalog count"))
        .unwrap_or(10_000);
    assert!(matches!(count, 10_000 | 100_000));
    let environment = support::Environment::start(Some(count), false);
    let origin = environment.origin.clone();
    let root = environment.root.clone();
    let evidence = environment.evidence.clone();
    let ui = AppWindow::new().expect("native Slint window");
    let (bridge, worker) = bridge::Bridge::start_with_platform(
        &ui,
        CoreConfig {
            paths: SettingsPaths {
                settings_file: root.join("settings.json"),
            },
            aria2_executable: "aria2c".into(),
        },
        Ok(hfdx_core::notification::NativePlatform::new(None)),
    )
    .expect("actual core");
    let snapshot = bridge.mailbox.lock().expect("mailbox").snapshot.clone();
    let files = files::FilesModel::new(snapshot.selection.clone());
    ui.set_rows(slint::ModelRc::from(files.clone()));
    view::initialize(&ui, &snapshot);
    callbacks::connect(&ui, &bridge, &files);
    ui.set_endpoint(origin.into());
    ui.set_allow_http(true);
    ui.set_repository("org/model".into());
    ui.show().expect("show native window");
    let first = ui.window().take_snapshot().expect("first actual render");
    let first_render_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("epoch")
        .as_nanos();
    let (rss, hwm, ticks) = resources();
    let mut csv = std::fs::File::create(evidence.join("samples.csv")).expect("samples");
    writeln!(
        csv,
        "kind,index,revision,matches,selected,snapshot_us,render_us,rss_kib,hwm_kib,cpu_ticks"
    )
    .expect("header");
    std::fs::write(evidence.join("startup.txt"), format!(
        "pid={}\nlaunch_basis={}\nlaunch_to_first_render_us={}\ntest_entry_to_first_render_us={}\nfirst_width={}\nfirst_height={}\nscale_factor={}\nrss_kib={}\nhwm_kib={}\ncpu_ticks={}\ncache=OS cache warm; fresh process; test binary not cargo\ninput=programmatic text property + production callback, not native keyboard\n",
        std::process::id(), launch_override.as_ref().map_or("test entry", |_| "process launch"), (first_render_ns-launch_ns)/1000, entered.elapsed().as_micros(), first.width(), first.height(), ui.window().scale_factor(), rss, hwm, ticks)).expect("startup evidence");
    let state = Rc::new(RefCell::new(Measurement {
        index: 0,
        revision: snapshot.revision,
        started: Instant::now(),
        csv,
    }));
    let b = bridge.clone();
    let weak = ui.as_weak();
    let measured = state.clone();
    let directory = evidence.clone();
    ui.on_refresh(move || {
        let ui = weak.upgrade().expect("live window");
        bridge::refresh_ui(&ui, &b, &files);
        let snapshot = b.mailbox.lock().expect("snapshot").snapshot.clone();
        assert!(snapshot.error.is_none(), "core error {:?}", snapshot.error);
        let mut state = measured.borrow_mut();
        if state.index == 31 || snapshot.revision <= state.revision {
            return;
        }
        if state.index == 0 {
            if snapshot.phase != UiPhase::Browsing {
                return;
            }
            assert_eq!(
                snapshot.selection.catalog.as_ref().expect("catalog").len(),
                count
            );
        } else if state.index <= 20 {
            let expected = if state.index % 2 == 1 { "path-000" } else { "" };
            if snapshot.selection.filter != expected {
                return;
            }
            assert_eq!(
                snapshot.selection.matches.len(),
                if expected.is_empty() { count } else { 1000 }
            );
        } else {
            let selected = if state.index % 2 == 1 { count } else { 0 };
            if snapshot.selection.summary.selected_count != selected {
                return;
            }
        }
        let snapshot_us = state.started.elapsed().as_micros();
        // The mailbox may advance after refresh_ui; measure once the rendered model catches up.
        if files.row_count() != snapshot.selection.visible.len() {
            return;
        }
        if let Some(row) = files.row_data(0)
            && row.selected != (snapshot.selection.summary.selected_count > 0)
        {
            return;
        }
        ui.window().request_redraw();
        slint::platform::update_timers_and_animations();
        let pixels = ui
            .window()
            .take_snapshot()
            .expect("render updated FilesModel");
        let render_us = state.started.elapsed().as_micros();
        let (rss, hwm, ticks) = resources();
        let kind = match state.index {
            0 => "load",
            1..=20 => "filter",
            _ => {
                if state.index % 2 == 1 {
                    "select"
                } else {
                    "deselect"
                }
            }
        };
        let index = state.index;
        writeln!(
            state.csv,
            "{kind},{index},{},{},{},{snapshot_us},{render_us},{rss},{hwm},{ticks}",
            snapshot.revision,
            snapshot.selection.matches.len(),
            snapshot.selection.summary.selected_count
        )
        .expect("sample");
        state.csv.flush().expect("flush samples");
        if state.index == 30 {
            let mut image = std::io::BufWriter::new(
                std::fs::File::create(directory.join("catalog.ppm")).expect("image"),
            );
            write!(image, "P6\n{} {}\n255\n", pixels.width(), pixels.height())
                .expect("image header");
            for pixel in pixels.as_slice() {
                image
                    .write_all(&[pixel.r, pixel.g, pixel.b])
                    .expect("pixel");
            }
            image.flush().expect("image flush");
            state.index = 31;
            drop(state);
            assert_eq!(b.close(&ui), slint::CloseRequestResponse::KeepWindowShown);
            return;
        }
        state.revision = snapshot.revision;
        state.index += 1;
        state.started = Instant::now();
        let next = state.index;
        drop(state);
        if next <= 20 {
            let text = if next % 2 == 1 { "path-000" } else { "" };
            ui.set_filter(text.into());
            ui.invoke_filter_edited(text.into());
        } else {
            ui.invoke_selection_action(if next % 2 == 1 { 0 } else { 1 });
        }
    });
    let (done_tx, done_rx) = mpsc::channel();
    let timed_out = Arc::new(AtomicBool::new(false));
    let timeout_flag = timed_out.clone();
    let watchdog = std::thread::spawn(move || {
        if matches!(
            done_rx.recv_timeout(Duration::from_secs(90)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            timeout_flag.store(true, Ordering::Release);
            slint::invoke_from_event_loop(|| slint::quit_event_loop().expect("bounded exit"))
                .expect("event loop");
        }
    });
    state.borrow_mut().started = Instant::now();
    ui.invoke_load();
    ui.run().expect("native loop");
    let deadline_elapsed = timed_out.load(Ordering::Acquire);
    if !deadline_elapsed {
        done_tx.send(()).expect("cancel deadline");
    }
    watchdog.join().expect("deadline joined");
    let stalled_index = state.borrow().index;
    let shutdown = if deadline_elapsed {
        bridge.handle.request_shutdown()
    } else {
        Ok(())
    };
    let core = worker.join().expect("bridge joined");
    shutdown.expect("bounded shutdown");
    core.expect("clean core shutdown");
    assert!(
        !deadline_elapsed,
        "measurement stalled at index {stalled_index}"
    );
    assert_eq!(
        state.borrow().index,
        31,
        "20 filter + five select/deselect pairs"
    );
    assert!(bridge.mailbox.lock().expect("mailbox").finished);
    println!("GUI performance catalog={count} samples=31 core joined");
    drop(state);
    environment.finish();
}
