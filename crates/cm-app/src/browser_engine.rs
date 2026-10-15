//! Secure CEF offscreen browser engine and runtime implementation.
//!
//! Provides the global CEF lifecycle, message pumping, offscreen software
//! BGRA rendering, input dispatch, navigation, and native download tracking.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use cef::rc::Rc;
// CEF wrapper macros require the generated Impl/Wrap traits in scope.
use cef::{args::Args, *};

use crate::browser_downloads::{finish_download, target_download_path, upsert_download_record};
use crate::browser_types::{
    BrowserFrame, BrowserKey, BrowserModifiers, BrowserMouseButton, BrowserSnapshot, DownloadStatus,
};

/// Global pump schedule delay requested by CEF (in milliseconds).
static NEXT_PUMP_DELAY_MS: AtomicI64 = AtomicI64::new(16);

/// Global flag marking whether CEF has been initialized.
static CEF_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Global registry of active browsers and engine controllers for graceful shutdown.
static ENGINE_REGISTRY: OnceLock<Mutex<Vec<Arc<EngineInner>>>> = OnceLock::new();

fn get_engine_registry() -> &'static Mutex<Vec<Arc<EngineInner>>> {
    ENGINE_REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Dispatches CEF child process execution when invoked by Chromium helper processes.
/// Returns Some(exit_code) if this process was a Chromium child process and should terminate,
/// or None if this is the main browser host application process.
pub fn subprocess_exit() -> Option<i32> {
    cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let args = Args::new();
    let mut app: Option<cef::App> = None;
    let code = execute_process(
        Some(args.as_main_args()),
        app.as_mut(),
        std::ptr::null_mut(),
    );
    if code >= 0 { Some(code) } else { None }
}

// App wrapper to hook command-line flags and schedule callback
wrap_app! {
    struct CmCefApp {
        process_handler: BrowserProcessHandler,
    }

    impl App {
        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(self.process_handler.clone())
        }

        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut cef::CommandLine>,
        ) {
            if let Some(cmd) = command_line {
                // Headless Ozone provides a display backend; Chromium's separate
                // --headless mode must not replace CEF's Alloy offscreen view.
                let ozone_platform = CefString::from("ozone-platform");
                let headless_val = CefString::from("headless");
                cmd.append_switch_with_value(Some(&ozone_platform), Some(&headless_val));

                // Disable GPU acceleration if not needed for headless OSR stability
                let disable_gpu = CefString::from("disable-gpu");
                cmd.append_switch(Some(&disable_gpu));
            }
        }
    }
}

wrap_browser_process_handler! {
    struct CmProcessHandler {}

    impl BrowserProcessHandler {
        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            // CEF schedule requests delay in ms. Bound minimum to avoid 0ms spin loop.
            let clamped = delay_ms.clamp(4, 500);
            NEXT_PUMP_DELAY_MS.store(clamped, Ordering::Relaxed);
        }
    }
}

/// Global Browser Runtime owner.
pub struct BrowserRuntime;

impl BrowserRuntime {
    /// Initializes CEF runtime once globally.
    pub fn initialize() -> Result<Self> {
        cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
        if CEF_INITIALIZED.load(Ordering::SeqCst) {
            return Ok(BrowserRuntime);
        }

        let runtime_path = std::env::var("CM_RS_CEF_PATH")
            .or_else(|_| std::env::var("CEF_PATH"))
            .ok()
            .or_else(|| option_env!("CEF_PATH").map(str::to_owned))
            .or_else(|| {
                std::env::current_exe()
                    .ok()?
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
            })
            .ok_or_else(|| anyhow!("Cannot locate the CEF runtime; set CM_RS_CEF_PATH"))?;

        let rt_dir = Path::new(&runtime_path);
        if !rt_dir.exists() {
            return Err(anyhow!(
                "CEF runtime directory does not exist: {}",
                runtime_path
            ));
        }

        // Setup persistent cache and root cache paths in XDG data/cache
        let cache_base = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("cm-rs")
            .join("browser");
        let profile = cache_base.join("profile");
        std::fs::create_dir_all(&profile).context("Cannot create browser profile")?;

        let mut settings = Settings::default();
        settings.windowless_rendering_enabled = 1;
        settings.external_message_pump = 1;
        settings.multi_threaded_message_loop = 0;
        settings.persist_session_cookies = 1;
        settings.log_severity = LogSeverity::WARNING;

        // Keep Chromium sandboxing enabled; missing support is an error, never
        // permission to expose an unsandboxed internet-facing browser.
        settings.no_sandbox = 0;

        settings.resources_dir_path = CefString::from(rt_dir.to_string_lossy().as_ref());
        settings.locales_dir_path =
            CefString::from(rt_dir.join("locales").to_string_lossy().as_ref());
        settings.cache_path = CefString::from(profile.to_string_lossy().as_ref());
        settings.root_cache_path = CefString::from(cache_base.to_string_lossy().as_ref());

        let process_handler = CmProcessHandler::new();
        let mut app = CmCefApp::new(process_handler);
        let args = Args::new();

        let ok = initialize(
            Some(args.as_main_args()),
            Some(&settings),
            Some(&mut app),
            std::ptr::null_mut(),
        );

        if ok != 1 {
            return Err(anyhow!(
                "Failed to initialize CEF engine (status code {ok})"
            ));
        }

        CEF_INITIALIZED.store(true, Ordering::SeqCst);
        Ok(BrowserRuntime)
    }

    /// Performs one iteration of message pump work.
    pub fn pump(&self) {
        if CEF_INITIALIZED.load(Ordering::SeqCst) {
            do_message_loop_work();
        }
    }

    /// Returns the recommended duration to sleep before the next message pump.
    /// Never returns 0 to prevent busy-spinning the UI loop.
    pub fn next_pump_delay(&self) -> Duration {
        let ms = NEXT_PUMP_DELAY_MS.load(Ordering::Relaxed).max(4);
        Duration::from_millis(ms as u64)
    }

    /// Closes all active browser engines and shuts down the CEF runtime safely.
    pub fn shutdown(&self) {
        if CEF_INITIALIZED.swap(false, Ordering::SeqCst) {
            let engines: Vec<_> = get_engine_registry()
                .lock()
                .map(|mut registry| registry.drain(..).collect())
                .unwrap_or_default();
            for inner in &engines {
                inner.close();
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while engines.iter().any(|inner| inner.browser_handle().is_some()) {
                do_message_loop_work();
                if std::time::Instant::now() >= deadline {
                    eprintln!("CEF close callbacks did not complete; skipping unsafe shutdown");
                    return;
                }
                std::thread::sleep(Duration::from_millis(4));
            }
            for inner in &engines {
                if let Ok(mut callbacks) = inner.download_callbacks.lock() {
                    callbacks.clear();
                }
            }
            shutdown();
        }
    }
}

/// Internal shared state between CEF callbacks and the BrowserEngine frontend.
pub struct EngineInner {
    browser: Mutex<Option<Browser>>,
    snapshot: Mutex<BrowserSnapshot>,
    latest_frame: Mutex<Option<BrowserFrame>>,
    view_width: Mutex<u32>,
    view_height: Mutex<u32>,
    scale_factor: Mutex<f32>,
    download_callbacks: Mutex<HashMap<u32, DownloadItemCallback>>,
    is_closed: AtomicBool,
    new_tabs: Mutex<Vec<String>>,
}

impl EngineInner {
    fn new(initial_url: &str) -> Self {
        let mut snap = BrowserSnapshot::default();
        snap.url = initial_url.to_string();
        snap.loading = true;

        Self {
            browser: Mutex::new(None),
            snapshot: Mutex::new(snap),
            latest_frame: Mutex::new(None),
            view_width: Mutex::new(1280),
            view_height: Mutex::new(800),
            scale_factor: Mutex::new(1.0),
            download_callbacks: Mutex::new(HashMap::new()),
            is_closed: AtomicBool::new(false),
            new_tabs: Mutex::new(Vec::new()),
        }
    }

    fn browser_handle(&self) -> Option<Browser> {
        self.browser.lock().ok().and_then(|guard| guard.clone())
    }

    fn close(&self) {
        if self.is_closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let browser = self.browser_handle();
        if let Some(host) = browser.and_then(|browser| browser.host()) {
            host.close_browser(1);
        }
    }
}

// Client handler wrapping Render, Load, LifeSpan, and Download handlers
wrap_client! {
    struct CmClient {
        inner: Arc<EngineInner>,
        render_handler: RenderHandler,
        load_handler: LoadHandler,
        display_handler: DisplayHandler,
        life_span_handler: LifeSpanHandler,
        download_handler: DownloadHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render_handler.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load_handler.clone())
        }

        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display_handler.clone())
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span_handler.clone())
        }

        fn download_handler(&self) -> Option<DownloadHandler> {
            Some(self.download_handler.clone())
        }
    }
}

// Render handler copying borrowed paint buffer to owned BrowserFrame
wrap_render_handler! {
    struct CmRenderHandler {
        inner: Arc<EngineInner>,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(r) = rect {
                let w = *self.inner.view_width.lock().unwrap_or_else(|e| e.into_inner()) as i32;
                let h = *self.inner.view_height.lock().unwrap_or_else(|e| e.into_inner()) as i32;
                r.x = 0;
                r.y = 0;
                r.width = w.max(1);
                r.height = h.max(1);
            }
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            if type_ != PaintElementType::VIEW || buffer.is_null() || width <= 0 || height <= 0 {
                return;
            }

            let w = width as u32;
            let h = height as u32;
            let Some(byte_count) = (w as usize).checked_mul(h as usize).and_then(|n| n.checked_mul(4)) else { return; };

            let borrowed = unsafe { std::slice::from_raw_parts(buffer, byte_count) };
            let frame = BrowserFrame {
                width: w,
                height: h,
                pixels: borrowed.to_vec(),
            };

            // Bounded latest-frame slot: overwrite without unbounded queuing
            if let Ok(mut slot) = self.inner.latest_frame.lock() {
                *slot = Some(frame);
            }
        }
    }
}

// Load handler for title/loading/back/forward/error tracking
wrap_load_handler! {
    struct CmLoadHandler {
        inner: Arc<EngineInner>,
    }

    impl LoadHandler {
        fn on_loading_state_change(
            &self,
            _browser: Option<&mut Browser>,
            is_loading: ::std::os::raw::c_int,
            can_go_back: ::std::os::raw::c_int,
            can_go_forward: ::std::os::raw::c_int,
        ) {
            if let Ok(mut snap) = self.inner.snapshot.lock() {
                snap.loading = is_loading != 0;
                snap.can_go_back = can_go_back != 0;
                snap.can_go_forward = can_go_forward != 0;
            }
        }

        fn on_load_end(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _http_status_code: ::std::os::raw::c_int,
        ) {
            if let Some(f) = frame {
                if f.is_main() != 0 {
                    let url_str = CefString::from(&f.url()).to_string();
                    if let Ok(mut snap) = self.inner.snapshot.lock() {
                        snap.url = url_str;
                        snap.loading = false;
                        snap.error = None;
                    }
                }
            }
        }

        fn on_load_error(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            error_code: Errorcode,
            error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            // Ignore ERR_ABORTED (-3) as it happens normally on navigation redirects/cancellations
            if (error_code.get_raw() as i32) == -3 {
                return;
            }
            if let Some(f) = frame {
                if f.is_main() != 0 {
                    let err_msg = error_text
                        .map(|t| t.to_string())
                        .unwrap_or_else(|| format!("Network error {}", error_code.get_raw()));
                    let url_msg = failed_url
                        .map(|u| u.to_string())
                        .unwrap_or_default();

                    if let Ok(mut snap) = self.inner.snapshot.lock() {
                        snap.loading = false;
                        snap.error = Some(format!("{} ({})", err_msg, url_msg));
                    }
                }
            }
        }
    }
}

// Track committed addresses (including same-document changes) and page titles.
wrap_display_handler! {
    struct CmDisplayHandler {
        inner: Arc<EngineInner>,
    }

    impl DisplayHandler {
        fn on_address_change(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, url: Option<&CefString>) {
            if frame.is_some_and(|frame| frame.is_main() != 0)
                && let Some(url) = url
                && let Ok(mut snapshot) = self.inner.snapshot.lock()
            {
                snapshot.url = url.to_string();
            }
        }

        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            if let Some(title) = title
                && let Ok(mut snapshot) = self.inner.snapshot.lock()
            {
                snapshot.title = title.to_string();
            }
        }
    }
}

// Life span handler for popup interception and lifetime management
wrap_life_span_handler! {
    struct CmLifeSpanHandler {
        inner: Arc<EngineInner>,
    }

    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: ::std::os::raw::c_int,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&cef::PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<cef::DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            // Keep popups inside the tab strip, never in an unmanaged native window.
            if let Some(url) = target_url {
                let url = url.to_string();
                if !url.is_empty()
                    && let Ok(mut tabs) = self.inner.new_tabs.lock()
                {
                    tabs.push(url);
                }
            }
            1 // 1 cancels the popup window creation
        }

        fn on_after_created(&self, browser: Option<&mut Browser>) {
            if let Some(b) = browser {
                if let Ok(mut guard) = self.inner.browser.lock() {
                    *guard = Some(b.clone());
                }
            }
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            if let Ok(mut guard) = self.inner.browser.lock() {
                *guard = None;
            }
            if let Ok(mut registry) = get_engine_registry().lock() {
                registry.retain(|inner| !Arc::ptr_eq(inner, &self.inner));
            }
            if let Ok(mut callbacks) = self.inner.download_callbacks.lock() {
                callbacks.clear();
            }
        }
    }
}

// Download handler preserving cookies/auth and saving to unique XDG cache path
wrap_download_handler! {
    struct CmDownloadHandler {
        inner: Arc<EngineInner>,
    }

    impl DownloadHandler {
        fn can_download(
            &self,
            _browser: Option<&mut Browser>,
            _url: Option<&CefString>,
            _request_method: Option<&CefString>,
        ) -> ::std::os::raw::c_int {
            1
        }

        fn on_before_download(
            &self,
            _browser: Option<&mut Browser>,
            download_item: Option<&mut DownloadItem>,
            suggested_name: Option<&CefString>,
            callback: Option<&mut BeforeDownloadCallback>,
        ) -> ::std::os::raw::c_int {
            if let (Some(item), Some(cb)) = (download_item, callback) {
                let id = item.id();
                let name = suggested_name
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "download.bin".to_string());
                let dest = match target_download_path(id, &name) {
                    Ok(path) => path,
                    Err(error) => {
                        if let Ok(mut snapshot) = self.inner.snapshot.lock() {
                            snapshot.error = Some(format!("Cannot save download: {error}"));
                        }
                        return 1;
                    }
                };
                let dest_str = dest.to_string_lossy().to_string();
                let cef_path = CefString::from(dest_str.as_str());

                // Record initial state
                if let Ok(mut snap) = self.inner.snapshot.lock() {
                    upsert_download_record(
                        &mut snap.downloads,
                        id,
                        name,
                        dest,
                        item.received_bytes(),
                        item.total_bytes(),
                        DownloadStatus::Downloading,
                    );
                }

                // Continue download without showing system file dialog (0 = no dialog)
                cb.cont(Some(&cef_path), 0);
            }
            1
        }

        fn on_download_updated(
            &self,
            _browser: Option<&mut Browser>,
            download_item: Option<&mut DownloadItem>,
            callback: Option<&mut DownloadItemCallback>,
        ) {
            if let Some(item) = download_item {
                let id = item.id();

                if let Some(cb) = callback {
                    if let Ok(mut map) = self.inner.download_callbacks.lock() {
                        map.insert(id, cb.clone());
                    }
                }

                let status = if item.is_complete() != 0 {
                    DownloadStatus::Complete
                } else if item.is_canceled() != 0 {
                    DownloadStatus::Cancelled
                } else if item.is_interrupted() != 0 {
                    DownloadStatus::Interrupted
                } else {
                    DownloadStatus::Downloading
                };

                let path_buf = PathBuf::from(CefString::from(&item.full_path()).to_string());
                if status == DownloadStatus::Complete || status == DownloadStatus::Cancelled {
                    finish_download(&path_buf);
                }
                if status != DownloadStatus::Downloading {
                    if let Ok(mut callbacks) = self.inner.download_callbacks.lock() {
                        callbacks.remove(&id);
                    }
                }

                if let Ok(mut snap) = self.inner.snapshot.lock() {
                    upsert_download_record(
                        &mut snap.downloads,
                        id,
                        String::new(),
                        path_buf,
                        item.received_bytes(),
                        item.total_bytes(),
                        status,
                    );
                }
            }
        }
    }
}

/// Embedded browser engine instance owned by a view.
pub struct BrowserEngine {
    inner: Arc<EngineInner>,
}

impl BrowserEngine {
    /// Creates a new offscreen browser pointing to `url`.
    pub fn new(url: &str) -> Result<Self> {
        if !CEF_INITIALIZED.load(Ordering::SeqCst) {
            return Err(anyhow!("Browser runtime is not initialized"));
        }

        let inner = Arc::new(EngineInner::new(url));

        let render_handler = CmRenderHandler::new(inner.clone());
        let load_handler = CmLoadHandler::new(inner.clone());
        let display_handler = CmDisplayHandler::new(inner.clone());
        let life_span_handler = CmLifeSpanHandler::new(inner.clone());
        let download_handler = CmDownloadHandler::new(inner.clone());

        let mut client = CmClient::new(
            inner.clone(),
            render_handler,
            load_handler,
            display_handler,
            life_span_handler,
            download_handler,
        );

        let window_info = WindowInfo::default().set_as_windowless(Default::default());
        let browser_settings = BrowserSettings::default();
        let target_url = CefString::from(url);

        let browser = browser_host_create_browser_sync(
            Some(&window_info),
            Some(&mut client),
            Some(&target_url),
            Some(&browser_settings),
            None,
            None,
        );

        let browser = match browser {
            Some(b) => b,
            None => return Err(anyhow!("Failed to create CEF browser instance")),
        };

        if let Ok(mut guard) = inner.browser.lock() {
            *guard = Some(browser);
        }

        // Register in global registry for safe lifecycle shutdown
        if let Ok(mut registry) = get_engine_registry().lock() {
            registry.push(inner.clone());
        }

        Ok(Self { inner })
    }

    pub fn take_new_tabs(&self) -> Vec<String> {
        self.inner
            .new_tabs
            .lock()
            .map(|mut tabs| std::mem::take(&mut *tabs))
            .unwrap_or_default()
    }

    pub fn snapshot(&self) -> BrowserSnapshot {
        self.inner
            .snapshot
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    pub fn take_frame(&self) -> Option<BrowserFrame> {
        self.inner
            .latest_frame
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
    }

    pub fn navigate(&self, url: &str) -> Result<()> {
        let browser = self.inner.browser_handle();
        if let Some(browser) = browser {
            if let Some(frame) = browser.main_frame() {
                let u = CefString::from(url);
                frame.load_url(Some(&u));
                if let Ok(mut snap) = self.inner.snapshot.lock() {
                    snap.url = url.to_string();
                    snap.loading = true;
                    snap.error = None;
                }
                return Ok(());
            }
        }
        Err(anyhow!("No active browser frame for navigation"))
    }

    pub fn back(&self) {
        if let Some(browser) = self.inner.browser_handle() {
            {
                browser.go_back();
            }
        }
    }

    pub fn forward(&self) {
        if let Some(browser) = self.inner.browser_handle() {
            {
                browser.go_forward();
            }
        }
    }

    pub fn reload(&self) {
        if let Some(browser) = self.inner.browser_handle() {
            {
                browser.reload();
            }
        }
    }

    pub fn stop(&self) {
        if let Some(browser) = self.inner.browser_handle() {
            {
                browser.stop_load();
            }
        }
    }

    pub fn resize(&self, width: u32, height: u32, scale: f32) {
        let w = width.max(1);
        let h = height.max(1);
        let s = if scale > 0.0 { scale } else { 1.0 };

        if let Ok(mut gw) = self.inner.view_width.lock() {
            *gw = w;
        }
        if let Ok(mut gh) = self.inner.view_height.lock() {
            *gh = h;
        }
        if let Ok(mut gs) = self.inner.scale_factor.lock() {
            *gs = s;
        }

        if let Some(browser) = self.inner.browser_handle() {
            {
                if let Some(host) = browser.host() {
                    host.was_resized();
                }
            }
        }
    }

    pub fn set_hidden(&self, hidden: bool) {
        if let Some(host) = self
            .inner
            .browser_handle()
            .and_then(|browser| browser.host())
        {
            host.was_hidden(i32::from(hidden));
        }
    }

    pub fn focus(&self, focused: bool) {
        if let Some(browser) = self.inner.browser_handle() {
            {
                if let Some(host) = browser.host() {
                    host.set_focus(if focused { 1 } else { 0 });
                }
            }
        }
    }

    pub fn mouse_move(&self, x: i32, y: i32, leave: bool, modifiers: BrowserModifiers) {
        if let Some(browser) = self.inner.browser_handle() {
            {
                if let Some(host) = browser.host() {
                    let mut evt = MouseEvent::default();
                    evt.x = x;
                    evt.y = y;
                    evt.modifiers = to_cef_modifiers(modifiers);
                    host.send_mouse_move_event(Some(&evt), if leave { 1 } else { 0 });
                }
            }
        }
    }

    pub fn mouse_button(
        &self,
        x: i32,
        y: i32,
        button: BrowserMouseButton,
        up: bool,
        clicks: i32,
        modifiers: BrowserModifiers,
    ) {
        if let Some(browser) = self.inner.browser_handle() {
            {
                if let Some(host) = browser.host() {
                    let mut evt = MouseEvent::default();
                    evt.x = x;
                    evt.y = y;
                    evt.modifiers = to_cef_modifiers(modifiers);

                    let btn_type = match button {
                        BrowserMouseButton::Left => MouseButtonType::LEFT,
                        BrowserMouseButton::Middle => MouseButtonType::MIDDLE,
                        BrowserMouseButton::Right => MouseButtonType::RIGHT,
                    };

                    host.send_mouse_click_event(
                        Some(&evt),
                        btn_type,
                        if up { 1 } else { 0 },
                        clicks.max(1),
                    );
                }
            }
        }
    }

    pub fn scroll(&self, x: i32, y: i32, dx: i32, dy: i32, modifiers: BrowserModifiers) {
        if let Some(browser) = self.inner.browser_handle() {
            {
                if let Some(host) = browser.host() {
                    let mut evt = MouseEvent::default();
                    evt.x = x;
                    evt.y = y;
                    evt.modifiers = to_cef_modifiers(modifiers);
                    host.send_mouse_wheel_event(Some(&evt), dx, dy);
                }
            }
        }
    }

    pub fn key(&self, event: BrowserKey) {
        if let Some(browser) = self.inner.browser_handle() {
            {
                if let Some(host) = browser.host() {
                    let mut key_evt = KeyEvent::default();
                    key_evt.type_ = if event.down {
                        KeyEventType::RAWKEYDOWN
                    } else {
                        KeyEventType::KEYUP
                    };
                    key_evt.modifiers = to_cef_modifiers(event.modifiers);
                    key_evt.windows_key_code = map_key_code(&event.key);

                    if matches!(event.key.as_str(), "enter" | "return") {
                        key_evt.character = 0x0D;
                        key_evt.unmodified_character = 0x0D;
                    } else if let Some(t) = event.text {
                        if let Some(ch) = t.encode_utf16().next() {
                            key_evt.character = ch;
                            key_evt.unmodified_character = ch;
                        }
                    }

                    host.send_key_event(Some(&key_evt));

                    // Text is delivered separately by text(), exactly once.
                }
            }
        }
    }

    pub fn text(&self, text: &str) {
        if let Some(browser) = self.inner.browser_handle() {
            {
                if let Some(host) = browser.host() {
                    for ch in text.encode_utf16() {
                        let mut evt = KeyEvent::default();
                        evt.type_ = KeyEventType::CHAR;
                        evt.character = ch;
                        evt.unmodified_character = ch;
                        if ch == 0x0D {
                            evt.windows_key_code = 0x0D;
                        }
                        host.send_key_event(Some(&evt));
                    }
                }
            }
        }
    }

    pub fn cancel_download(&self, id: u32) {
        let callback = self
            .inner
            .download_callbacks
            .lock()
            .ok()
            .and_then(|mut map| map.remove(&id));
        if let Some(callback) = callback {
            callback.cancel();
        }
        // Only CEF's terminal callback can declare cancellation successful.
    }

    pub fn close(&self) {
        self.inner.close();
    }
}

impl Drop for BrowserEngine {
    fn drop(&mut self) {
        self.inner.close();
    }
}

/// Converts generic browser modifiers to CEF event flags.
fn to_cef_modifiers(m: BrowserModifiers) -> u32 {
    let mut flags = 0u32;
    if m.shift {
        flags |= 1 << 1; // EVENTFLAG_SHIFT_DOWN
    }
    if m.control {
        flags |= 1 << 2; // EVENTFLAG_CONTROL_DOWN
    }
    if m.alt {
        flags |= 1 << 3; // EVENTFLAG_ALT_DOWN
    }
    if m.left_button {
        flags |= 1 << 4; // EVENTFLAG_LEFT_MOUSE_BUTTON
    }
    if m.middle_button {
        flags |= 1 << 5; // EVENTFLAG_MIDDLE_MOUSE_BUTTON
    }
    if m.right_button {
        flags |= 1 << 6; // EVENTFLAG_RIGHT_MOUSE_BUTTON
    }
    if m.command {
        flags |= 1 << 7; // EVENTFLAG_COMMAND_DOWN
    }
    flags
}

/// Maps common GPUI key identifiers to Windows virtual key codes expected by CEF.
fn map_key_code(key: &str) -> i32 {
    match key {
        "backspace" => 0x08,
        "tab" => 0x09,
        "enter" | "return" => 0x0D,
        "shift" => 0x10,
        "control" => 0x11,
        "alt" => 0x12,
        "escape" => 0x1B,
        "space" => 0x20,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "end" => 0x23,
        "home" => 0x24,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "delete" => 0x2E,
        s if s.len() == 1 => {
            let c = s.chars().next().unwrap();
            if c.is_ascii_alphabetic() {
                c.to_ascii_uppercase() as i32
            } else if c.is_ascii_digit() {
                c as i32
            } else {
                0
            }
        }
        _ => 0,
    }
}
