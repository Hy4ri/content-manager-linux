mod app;
mod browser_downloads;
mod browser_engine;
mod browser_tabs;
mod browser_types;
mod browser_view;
mod cli;
mod content_sort;
mod csp_view;
mod events_view;
mod mods_view;
mod nav;
mod online_view;
mod text_input;
mod theme;
mod widgets;

use gpui::{
    App, Application, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions, actions,
    prelude::*, px, size,
};

actions!(cm, [Quit]);

fn main() {
    // Chromium child processes must be dispatched before the app's CLI parser.
    if let Some(code) = browser_engine::subprocess_exit() {
        std::process::exit(code);
    }
    // `cm-rs <command> ...` runs the command-line tools instead of the GUI.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        std::process::exit(cli::run(&args));
    }

    let runtime = match browser_engine::BrowserRuntime::initialize() {
        Ok(runtime) => Some(std::rc::Rc::new(runtime)),
        Err(error) => {
            eprintln!("Embedded browser unavailable: {error:#}");
            None
        }
    };
    Application::new().run(move |cx: &mut App| {
        if let Some(runtime) = runtime {
            let pump = runtime.clone();
            let timer = cx.background_executor().clone();
            cx.spawn(async move |_cx| {
                loop {
                    pump.pump();
                    timer.timer(pump.next_pump_delay()).await;
                }
            })
            .detach();
            cx.on_app_quit(move |_| {
                runtime.shutdown();
                async {}
            })
            .detach();
        }
        text_input::bind_keys(cx);
        cx.bind_keys([KeyBinding::new("secondary-q", Quit, None)]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1360.), px(860.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Content Manager (Rust)".into()),
                    ..Default::default()
                }),
                app_id: Some("cm-rs".into()),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    #[allow(unused_mut)]
                    let mut app = app::CmApp::new(cx);
                    #[cfg(target_os = "linux")]
                    app.start_pad_nav(window, cx);
                    app.focus_root(window);
                    app
                })
            },
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
