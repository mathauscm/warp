use std::cell::RefCell;
use std::ffi::c_void;
use std::path::Path;

use anyhow::{Context as _, Result, anyhow};
use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{
    ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
    NSBitmapImageFileType, NSBitmapImageRep, NSColor, NSImage, NSWorkspace,
};
use objc2_foundation::{
    NSDictionary, NSError, NSKeyValueObservingOptions, NSNumber, NSObject,
    NSObjectNSKeyValueCoding, NSObjectNSKeyValueObserverRegistration, NSPoint, NSRect, NSSize,
    NSString, NSURL, NSURLRequest, ns_string,
};
use objc2_web_kit::{
    WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate,
    WKNavigationType, WKUIDelegate, WKWebView, WKWebViewConfiguration, WKWindowFeatures,
};

use crate::{NavigationState, WebViewEvent};

/// The native view a [`WebView`] is attached to (typically the window's content view).
pub type ParentView = objc2_app_kit::NSView;

/// Schemes the page may navigate to inside the web view.
const LOCAL_SCHEMES: &[&str] = &["file", "about", "data", "blob"];

/// Schemes that may be handed to the system when the page asks to open them. Anything else is
/// dropped so untrusted HTML can't launch arbitrary URL-scheme handlers.
const EXTERNAL_SCHEMES: &[&str] = &["http", "https", "mailto"];

/// Schemes a browsing web view loads itself.
const BROWSER_SCHEMES: &[&str] = &["http", "https", "file", "about", "data", "blob"];

/// The `WKWebView` properties a browsing web view observes to report [`WebViewEvent::StateChanged`].
const OBSERVED_KEY_PATHS: &[&str] = &["title", "URL", "canGoBack", "canGoForward", "loading"];

/// `NSURLErrorCancelled`: a load replaced by another one, which isn't worth reporting.
const URL_ERROR_CANCELLED: isize = -999;
/// `WebKitErrorFrameLoadInterruptedByPolicyChange`: a load the navigation policy cancelled.
const FRAME_LOAD_INTERRUPTED: isize = 102;

/// Without a Safari version in the user agent, some sites serve degraded pages to WKWebView.
const APPLICATION_NAME_FOR_USER_AGENT: &str = "Version/18.0 Safari/605.1.15";

/// A WKWebView attached as a child of a native parent view. Main-thread only (`!Send`).
pub struct WebView {
    web_view: Retained<WKWebView>,
    // WKWebView only holds its delegates weakly.
    delegate: Retained<NavigationDelegate>,
}

impl WebView {
    /// A web view for local artifacts: it stays on local content and opens web links in the
    /// default browser.
    pub fn new(parent: &ParentView) -> Result<Self> {
        Self::with_mode(parent, None)
    }

    /// A web view that browses the web like a browser tab, reporting its navigation through
    /// `on_event`.
    pub fn new_browser(
        parent: &ParentView,
        on_event: impl Fn(WebViewEvent) + 'static,
    ) -> Result<Self> {
        let this = Self::with_mode(parent, Some(Box::new(on_event)))?;
        for key_path in OBSERVED_KEY_PATHS {
            // SAFETY: the observer is removed in `Drop`, while `Self` still keeps it alive.
            unsafe {
                this.web_view.addObserver_forKeyPath_options_context(
                    &this.delegate,
                    &NSString::from_str(key_path),
                    NSKeyValueObservingOptions::New,
                    std::ptr::null_mut(),
                );
            }
        }
        Ok(this)
    }

    fn with_mode(parent: &ParentView, on_event: Option<Box<dyn Fn(WebViewEvent)>>) -> Result<Self> {
        let mtm = MainThreadMarker::new().context("WebView must be created on the main thread")?;
        let is_browser = on_event.is_some();

        // SAFETY: plain Objective-C object construction and property setters on the main thread.
        // The KVC keys are long-standing WKPreferences/WKWebView properties (also used by wry).
        let web_view = unsafe {
            let configuration = WKWebViewConfiguration::new(mtm);
            if is_browser {
                configuration.setApplicationNameForUserAgent(Some(&NSString::from_str(
                    APPLICATION_NAME_FOR_USER_AGENT,
                )));
            }
            let preferences = configuration.preferences();
            preferences.setJavaScriptCanOpenWindowsAutomatically(false);
            let yes = NSNumber::numberWithBool(true);
            preferences.setValue_forKey(Some(&yes), ns_string!("developerExtrasEnabled"));
            preferences.setValue_forKey(Some(&yes), ns_string!("allowFileAccessFromFileURLs"));

            let web_view = WKWebView::initWithFrame_configuration(
                WKWebView::alloc(mtm),
                NSRect::ZERO,
                &configuration,
            );
            // Lets the GPU-rendered placeholder show through until the page paints, avoiding a
            // white flash in dark mode.
            web_view.setValue_forKey(
                Some(&NSNumber::numberWithBool(false)),
                ns_string!("drawsBackground"),
            );
            if web_view.respondsToSelector(sel!(setInspectable:)) {
                web_view.setInspectable(true);
            }
            if is_browser {
                web_view.setAllowsBackForwardNavigationGestures(true);
                web_view.setAllowsMagnification(true);
            }
            web_view
        };

        let delegate = NavigationDelegate::new(mtm, on_event);
        // SAFETY: the delegate is kept alive by `Self` for as long as it is installed.
        unsafe {
            web_view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            web_view.setUIDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        }
        parent.addSubview(&web_view);

        Ok(Self { web_view, delegate })
    }

    /// Positions the view using WarpUI's top-left-origin logical coordinates.
    pub fn set_frame(&self, x: f32, y: f32, width: f32, height: f32, parent_height: f32) {
        // SAFETY: reading the superview of a live view on the main thread.
        let parent_is_flipped = unsafe { self.web_view.superview() }.is_some_and(|v| v.isFlipped());
        let origin_y = if parent_is_flipped {
            y
        } else {
            parent_height - y - height
        };
        self.web_view.setFrame(NSRect::new(
            NSPoint::new(x.into(), origin_y.into()),
            NSSize::new(width.max(0.).into(), height.max(0.).into()),
        ));
    }

    pub fn set_visible(&self, visible: bool) {
        self.web_view.setHidden(!visible);
    }

    /// Sets the color shown behind the page (overscroll areas and transparent page backgrounds).
    pub fn set_background(&self, r: f32, g: f32, b: f32, a: f32) {
        let color =
            NSColor::colorWithSRGBRed_green_blue_alpha(r.into(), g.into(), b.into(), a.into());
        // SAFETY: setter on a live view on the main thread.
        unsafe { self.web_view.setUnderPageBackgroundColor(Some(&color)) };
    }

    /// Forces the page's `prefers-color-scheme` and native controls to dark or light.
    pub fn set_dark_appearance(&self, dark: bool) {
        // SAFETY: the appearance-name statics are provided by AppKit.
        let name = unsafe {
            if dark {
                NSAppearanceNameDarkAqua
            } else {
                NSAppearanceNameAqua
            }
        };
        let appearance = NSAppearance::appearanceNamed(name);
        self.web_view.setAppearance(appearance.as_deref());
    }

    pub fn load_file(&self, html_path: &Path, read_access_dir: &Path) {
        let (Some(url), Some(read_access)) = (
            NSURL::from_file_path(html_path),
            NSURL::from_directory_path(read_access_dir),
        ) else {
            log::warn!("Could not build file URLs for web view: {html_path:?}");
            return;
        };
        // SAFETY: both URLs are valid file URLs.
        unsafe {
            self.web_view
                .loadFileURL_allowingReadAccessToURL(&url, &read_access)
        };
    }

    pub fn load_html(&self, html: &str, base_url: Option<&Path>) {
        let base_url = base_url.and_then(NSURL::from_directory_path);
        // SAFETY: loading an in-memory string with an optional file base URL.
        unsafe {
            self.web_view
                .loadHTMLString_baseURL(&NSString::from_str(html), base_url.as_deref())
        };
    }

    /// Loads `url`, which must be absolute (like `http://localhost:5173/`).
    pub fn load_url(&self, url: &str) -> Result<()> {
        let ns_url = NSURL::URLWithString(&NSString::from_str(url))
            .with_context(|| format!("invalid URL: {url}"))?;
        let request = NSURLRequest::requestWithURL(&ns_url);
        // SAFETY: loading a request built from a valid URL.
        unsafe { self.web_view.loadRequest(&request) };
        Ok(())
    }

    pub fn reload(&self) {
        // SAFETY: plain method call on a live view.
        unsafe { self.web_view.reload() };
    }

    pub fn stop_loading(&self) {
        // SAFETY: plain method call on a live view.
        unsafe { self.web_view.stopLoading() };
    }

    pub fn go_back(&self) {
        // SAFETY: plain method call on a live view; it does nothing without history.
        unsafe { self.web_view.goBack() };
    }

    pub fn go_forward(&self) {
        // SAFETY: plain method call on a live view; it does nothing without history.
        unsafe { self.web_view.goForward() };
    }

    pub fn navigation_state(&self) -> NavigationState {
        // SAFETY: property getters on a live view on the main thread.
        unsafe {
            NavigationState {
                url: self
                    .web_view
                    .URL()
                    .and_then(|url| url.absoluteString())
                    .map(|url| url.to_string()),
                title: self
                    .web_view
                    .title()
                    .map(|title| title.to_string())
                    .filter(|title| !title.is_empty()),
                can_go_back: self.web_view.canGoBack(),
                can_go_forward: self.web_view.canGoForward(),
                loading: self.web_view.isLoading(),
            }
        }
    }

    pub fn evaluate_js(&self, script: &str) {
        let completion = RcBlock::new(|_result: *mut AnyObject, error: *mut NSError| {
            // SAFETY: WebKit passes either null or a valid NSError.
            if let Some(error) = unsafe { error.as_ref() } {
                log::warn!("Web view script failed: {}", error.localizedDescription());
            }
        });
        // SAFETY: the completion block matches the expected signature.
        unsafe {
            self.web_view.evaluateJavaScript_completionHandler(
                &NSString::from_str(script),
                Some(&completion),
            )
        };
    }

    /// Captures the visible page as a PNG file (one pixel per point; areas the page leaves
    /// unpainted stay transparent). `done` runs on the main thread once the file is written or
    /// capture fails.
    pub fn snapshot_png(&self, out_path: &Path, done: impl FnOnce(Result<()>) + 'static) {
        let out_path = out_path.to_path_buf();
        let done = RefCell::new(Some(done));
        let completion = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
            let Some(done) = done.borrow_mut().take() else {
                return;
            };
            // SAFETY: WebKit passes either null or valid objects.
            let (image, error) = unsafe { (image.as_ref(), error.as_ref()) };
            let result = match (image, error) {
                (Some(image), _) => write_png(image, &out_path),
                (None, Some(error)) => Err(anyhow!(
                    "web view snapshot failed: {}",
                    error.localizedDescription()
                )),
                (None, None) => Err(anyhow!("web view snapshot returned no image")),
            };
            done(result);
        });
        // SAFETY: the completion block matches the expected signature; a nil configuration
        // captures the view's visible bounds.
        unsafe {
            self.web_view
                .takeSnapshotWithConfiguration_completionHandler(None, &completion)
        };
    }

    /// Returns keyboard focus to the parent view if the web view (or its content) holds it.
    pub fn release_focus(&self) {
        let Some(window) = self.web_view.window() else {
            return;
        };
        let Some(first_responder) = window.firstResponder() else {
            return;
        };
        let focus_is_inside = first_responder
            .downcast_ref::<ParentView>()
            .is_some_and(|view| view.isDescendantOf(&self.web_view));
        if focus_is_inside {
            // SAFETY: reading the superview of a live view on the main thread.
            let superview = unsafe { self.web_view.superview() };
            window.makeFirstResponder(superview.as_deref().map(|view| view.as_super()));
        }
    }

    pub fn remove(&self) {
        self.web_view.removeFromSuperview();
    }
}

impl Drop for WebView {
    fn drop(&mut self) {
        if self.delegate.ivars().on_event.is_some() {
            for key_path in OBSERVED_KEY_PATHS {
                // SAFETY: the observer was added in `new_browser` and is still alive.
                unsafe {
                    self.web_view
                        .removeObserver_forKeyPath(&self.delegate, &NSString::from_str(key_path));
                }
            }
        }
        // SAFETY: clearing delegates and stopping loads on a live view on the main thread.
        unsafe {
            self.web_view.stopLoading();
            self.web_view.setNavigationDelegate(None);
            self.web_view.setUIDelegate(None);
        }
        self.remove();
    }
}

fn write_png(image: &NSImage, out_path: &Path) -> Result<()> {
    let tiff = image
        .TIFFRepresentation()
        .context("snapshot has no TIFF representation")?;
    let bitmap = NSBitmapImageRep::imageRepWithData(&tiff).context("could not decode snapshot")?;
    let properties = NSDictionary::<NSString, AnyObject>::new();
    // SAFETY: an empty property dictionary is valid for PNG encoding.
    let png = unsafe {
        bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &properties)
    }
    .context("could not encode snapshot as PNG")?;
    if let Some(dir) = out_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(out_path, png.to_vec())?;
    Ok(())
}

fn url_scheme(url: &NSURL) -> Option<String> {
    url.scheme()
        .map(|scheme| scheme.to_string().to_ascii_lowercase())
}

fn open_externally(url: &NSURL) {
    if url_scheme(url).is_some_and(|scheme| EXTERNAL_SCHEMES.contains(&scheme.as_str())) {
        NSWorkspace::sharedWorkspace().openURL(url);
    }
}

struct DelegateIvars {
    /// Set for a browsing web view, which reports its navigation through it.
    on_event: Option<Box<dyn Fn(WebViewEvent)>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and this type does not implement Drop.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "PicolasWebViewNavigationDelegate"]
    #[ivars = DelegateIvars]
    struct NavigationDelegate;

    impl NavigationDelegate {
        /// Key-value observing of the browsing web view's navigation properties.
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe_value(
            &self,
            _key_path: Option<&NSString>,
            _object: Option<&AnyObject>,
            _change: Option<&AnyObject>,
            _context: *mut c_void,
        ) {
            self.emit(WebViewEvent::StateChanged);
        }
    }

    unsafe impl NSObjectProtocol for NavigationDelegate {}

    unsafe impl WKNavigationDelegate for NavigationDelegate {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_policy_for_navigation_action(
            &self,
            _web_view: &WKWebView,
            navigation_action: &WKNavigationAction,
            decision_handler: &DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            let policy = if self.is_browser() {
                browser_navigation_policy(navigation_action)
            } else {
                navigation_policy(navigation_action)
            };
            decision_handler.call((policy,));
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish_navigation(&self, _web_view: &WKWebView, _navigation: Option<&WKNavigation>) {
            self.emit(WebViewEvent::StateChanged);
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn did_fail_navigation(
            &self,
            web_view: &WKWebView,
            _navigation: Option<&WKNavigation>,
            error: &NSError,
        ) {
            self.report_failure(web_view, error, None);
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn did_fail_provisional_navigation(
            &self,
            web_view: &WKWebView,
            _navigation: Option<&WKNavigation>,
            error: &NSError,
        ) {
            self.report_failure(web_view, error, failing_url(error));
        }
    }

    unsafe impl WKUIDelegate for NavigationDelegate {
        /// Pages asking for a new window (`target="_blank"`, `window.open`) get the URL loaded in
        /// place when browsing, and opened in the default browser otherwise.
        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn create_web_view(
            &self,
            web_view: &WKWebView,
            _configuration: &WKWebViewConfiguration,
            navigation_action: &WKNavigationAction,
            _window_features: &WKWindowFeatures,
        ) -> Option<Retained<WKWebView>> {
            // SAFETY: reading properties of a live navigation action.
            let request = unsafe { navigation_action.request() };
            if self.is_browser() {
                // SAFETY: loading the page's own request in the view that asked for it.
                unsafe { web_view.loadRequest(&request) };
            } else if let Some(url) = request.URL() {
                open_externally(&url);
            }
            None
        }
    }
);

impl NavigationDelegate {
    fn new(mtm: MainThreadMarker, on_event: Option<Box<dyn Fn(WebViewEvent)>>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars { on_event });
        // SAFETY: NSObject's `init` signature is correct.
        unsafe { msg_send![super(this), init] }
    }

    fn is_browser(&self) -> bool {
        self.ivars().on_event.is_some()
    }

    fn emit(&self, event: WebViewEvent) {
        if let Some(on_event) = &self.ivars().on_event {
            on_event(event);
        }
    }

    fn report_failure(&self, web_view: &WKWebView, error: &NSError, url: Option<String>) {
        if matches!(error.code(), URL_ERROR_CANCELLED | FRAME_LOAD_INTERRUPTED) {
            self.emit(WebViewEvent::StateChanged);
            return;
        }
        // SAFETY: reading a property of a live view on the main thread.
        let url = url.or_else(|| {
            unsafe { web_view.URL() }
                .and_then(|url| url.absoluteString())
                .map(|url| url.to_string())
        });
        self.emit(WebViewEvent::LoadFailed {
            url,
            message: error.localizedDescription().to_string(),
        });
    }
}

/// The URL a provisional navigation failed on, from the error's user info.
fn failing_url(error: &NSError) -> Option<String> {
    let user_info = error.userInfo();
    let value = user_info.objectForKey(ns_string!("NSErrorFailingURLStringKey"))?;
    let value = value.downcast_ref::<NSString>()?;
    Some(value.to_string())
}

/// Lets a browsing web view go anywhere on the web, handing other schemes (like `mailto:`) to the
/// system when allowed.
fn browser_navigation_policy(navigation_action: &WKNavigationAction) -> WKNavigationActionPolicy {
    // SAFETY: reading properties of a live navigation action.
    let Some(url) = (unsafe { navigation_action.request() }).URL() else {
        return WKNavigationActionPolicy::Allow;
    };
    if url_scheme(&url).is_some_and(|scheme| BROWSER_SCHEMES.contains(&scheme.as_str())) {
        return WKNavigationActionPolicy::Allow;
    }
    open_externally(&url);
    WKNavigationActionPolicy::Cancel
}

/// Keeps the web view on local content: local schemes and subframe loads are allowed, while
/// top-level or new-window navigations to anything else are cancelled, opening user-clicked
/// links in the default browser.
fn navigation_policy(navigation_action: &WKNavigationAction) -> WKNavigationActionPolicy {
    // SAFETY: reading properties of a live navigation action.
    let (url, target_frame, navigation_type) = unsafe {
        (
            navigation_action.request().URL(),
            navigation_action.targetFrame(),
            navigation_action.navigationType(),
        )
    };
    let Some(url) = url else {
        return WKNavigationActionPolicy::Allow;
    };
    if url_scheme(&url).is_some_and(|scheme| LOCAL_SCHEMES.contains(&scheme.as_str())) {
        return WKNavigationActionPolicy::Allow;
    }
    // SAFETY: reading a property of a live frame info.
    let targets_subframe = target_frame
        .as_ref()
        .is_some_and(|frame| !unsafe { frame.isMainFrame() });
    if targets_subframe {
        return WKNavigationActionPolicy::Allow;
    }
    if target_frame.is_none() || navigation_type == WKNavigationType::LinkActivated {
        open_externally(&url);
    }
    WKNavigationActionPolicy::Cancel
}
