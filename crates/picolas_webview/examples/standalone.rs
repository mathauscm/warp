//! Opens a window with a WKWebView showing a local HTML file, writes a PNG snapshot after two
//! seconds and quits. Set `PICOLAS_WEBVIEW_KEEP_OPEN=1` to keep the window open.

#[cfg(target_os = "macos")]
fn main() {
    use std::rc::Rc;

    use block2::RcBlock;
    use objc2::MainThreadOnly;
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSWindow,
        NSWindowStyleMask,
    };
    use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString, NSTimer};
    use picolas_webview::WebView;

    const HTML: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Picolas webview</title>
<style>
  :root { color-scheme: light dark; }
  body { font: 15px -apple-system, sans-serif; margin: 32px; }
  button { font-size: 15px; padding: 6px 14px; border-radius: 8px; }
</style></head>
<body>
  <h1>Hello from a local artifact</h1>
  <p>Clicks: <span id="count">0</span></p>
  <button onclick="count.textContent = Number(count.textContent) + 1">Click me</button>
  <p><a href="https://example.com" target="_blank">External link (opens in browser)</a></p>
</body></html>"#;

    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    // SAFETY: the window is kept alive for the whole run and not released on close.
    let window = unsafe {
        let window = NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(200., 200.), NSSize::new(800., 600.)),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable,
            NSBackingStoreType::Buffered,
            false,
        );
        window.setReleasedWhenClosed(false);
        window
    };
    window.setTitle(&NSString::from_str("picolas_webview standalone"));
    let content_view = window.contentView().expect("window has a content view");

    let dir = std::env::temp_dir().join("picolas_webview_example");
    std::fs::create_dir_all(&dir).expect("create example dir");
    let html_path = dir.join("index.html");
    std::fs::write(&html_path, HTML).expect("write example html");

    let web_view = Rc::new(WebView::new(&content_view).expect("create web view"));
    let parent_height = content_view.frame().size.height as f32;
    web_view.set_frame(20., 20., 760., 560., parent_height);
    web_view.set_dark_appearance(true);
    web_view.set_background(0.15, 0.15, 0.14, 1.);
    web_view.load_file(&html_path, &dir);
    window.makeKeyAndOrderFront(None);

    let snapshot_path = dir.join("snapshot.png");
    let keep_open = std::env::var_os("PICOLAS_WEBVIEW_KEEP_OPEN").is_some();
    let timer_web_view = Rc::clone(&web_view);
    let on_timer = RcBlock::new(move |_timer| {
        timer_web_view.evaluate_js("document.getElementById('count').textContent = '42'");
        let snapshot_path = snapshot_path.clone();
        timer_web_view.snapshot_png(&snapshot_path.clone(), move |result| {
            match &result {
                Ok(()) => println!("snapshot written to {}", snapshot_path.display()),
                Err(err) => eprintln!("snapshot failed: {err:#}"),
            }
            if !keep_open {
                std::process::exit(i32::from(result.is_err()));
            }
        });
    });
    // SAFETY: the block matches NSTimer's expected signature.
    let _timer =
        unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(2., false, &on_timer) };

    app.run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This example only runs on macOS.");
}
