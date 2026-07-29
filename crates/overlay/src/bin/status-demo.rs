//! Visual demo: puts the status item in the menu bar, cycles it through
//! every state, and prints each click, so a human can verify with their own
//! eyes that the glyph changes, the menu opens, and clicking a row is
//! delivered — without building and launching the whole daemon.
//!
//! Run: `cargo run -p overlay --bin status-demo`
//! Quit from the menu, or Ctrl-C it.

fn main() -> anyhow::Result<()> {
    #[cfg(all(target_os = "macos", feature = "display"))]
    return demo::run();

    #[cfg(not(all(target_os = "macos", feature = "display")))]
    {
        // Same contract as the overlay demo: unsupported is a clean,
        // explained exit, so this binary compiles and runs everywhere.
        eprintln!(
            "status-demo: unsupported here (needs macOS and the `display` feature). \
             The menu model itself is platform-neutral; see `overlay::menu`."
        );
        Ok(())
    }
}

#[cfg(all(target_os = "macos", feature = "display"))]
mod demo {
    use std::time::Instant;

    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSRunLoop};
    use overlay::menu::{MenuId, MenuItem, MenuModel};
    use overlay::status_item::MacStatusItem;
    use overlay::OverlayState;

    /// Seconds per state, long enough to see the glyph change.
    const STEP_SECS: f64 = 2.0;
    /// The id of the Quit row, which is the last one built below.
    const QUIT: MenuId = MenuId(1);

    fn model(state: OverlayState) -> MenuModel {
        MenuModel {
            state,
            tooltip: format!("OutLoud demo: {state}"),
            items: vec![
                MenuItem::Label(format!("State: {state}")),
                MenuItem::Separator,
                MenuItem::choice("A checked choice", MenuId(0), true),
                MenuItem::action("Quit the demo", QUIT),
            ],
        }
    }

    pub fn run() -> anyhow::Result<()> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| anyhow::anyhow!("status-demo must run on the main thread"))?;
        let app = NSApplication::sharedApplication(mtm);
        // Accessory, exactly like the daemon: a status item must work with
        // no Dock icon and without ever activating the app.
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        // finishLaunching, without run(): the status bar is not wired up
        // until AppKit considers the app launched, and this demo (like the
        // daemon) pumps the run loop itself instead of surrendering the
        // main thread to run().
        app.finishLaunching();

        let mut item = MacStatusItem::new(mtm)?;
        println!("status-demo: look at the right side of your menu bar.");

        let start = Instant::now();
        let run_loop = NSRunLoop::currentRunLoop();
        let mut last = usize::MAX;
        loop {
            let step = (start.elapsed().as_secs_f64() / STEP_SECS) as usize;
            let state = OverlayState::ALL[step % OverlayState::ALL.len()];
            if step != last {
                println!("  {state}");
                last = step;
            }
            item.apply(&model(state));
            for id in item.drain_clicks() {
                println!("  clicked {id:?}");
                if id == QUIT {
                    return Ok(());
                }
            }
            unsafe {
                let until = NSDate::dateWithTimeIntervalSinceNow(1.0 / 30.0);
                run_loop.runMode_beforeDate(NSDefaultRunLoopMode, &until);
            }
        }
    }
}
