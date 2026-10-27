use std::cell::RefCell;
use std::path::Path;

use anyhow::{Context as _, Result, anyhow};
use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
    NSBitmapImageFileType, NSBitmapImageRep, NSColor, NSImage, NSWorkspace,
};
use objc2_foundation::{
    NSDictionary, NSError, NSNumber, NSObject, NSObjectNSKeyValueCoding, NSPoint, NSRect, NSSize,
    NSString, NSURL, ns_string,
};
use objc2_web_kit::{
    WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKNavigationType,
    WKUIDelegate, WKWebView, WKWebViewConfiguration, WKWindowFeatures,
};

/// The native view a [`WebView`] is attached to (typically the window's content view).
pub type ParentView = objc2_app_kit::NSView;

/// Schemes the page may navigate to inside the web view.
const LOCAL_SCHEMES: &[&str] = &["file", "about", "data", "blob"];

/// Schemes that may be handed to the system when the page asks to open them. Anything else is
/// dropped so untrusted HTML can't launch arbitrary URL-scheme handlers.
const EXTERNAL_SCHEMES: &[&str] = &["http", "https", "mailto"];

/// A WKWebView attached as a child of a native parent view. Main-thread only (`!Send`).
pub struct WebView {
    web_view: Retained<WKWebView>,
    // WKWebView only holds its delegates weakly.
    _delegate: Retained<NavigationDelegate>,
}

impl WebView {
    pub fn new(parent: &ParentView) -> Result<Self> {
        let mtm = MainThreadMarker::new().context("WebView must be created on the main thread")?;

        // SAFETY: plain Objective-C object construction and property setters on the main thread.
        // The KVC keys are long-standing WKPreferences/WKWebView properties (also used by wry).
        let web_view = unsafe {
            let configuration = WKWebViewConfiguration::new(mtm);
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
            web_view
        };

        let delegate = NavigationDelegate::new(mtm);
        // SAFETY: the delegate is kept alive by `Self` for as long as it is installed.
        unsafe {
            web_view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            web_view.setUIDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        }
        parent.addSubview(&web_view);

        Ok(Self {
            web_view,
            _delegate: delegate,
        })
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

    pub fn reload(&self) {
        // SAFETY: plain method call on a live view.
        unsafe { self.web_view.reload() };
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

define_class!(
    // SAFETY: NSObject has no subclassing requirements and this type does not implement Drop.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "PicolasWebViewNavigationDelegate"]
    struct NavigationDelegate;

    unsafe impl NSObjectProtocol for NavigationDelegate {}

    unsafe impl WKNavigationDelegate for NavigationDelegate {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_policy_for_navigation_action(
            &self,
            _web_view: &WKWebView,
            navigation_action: &WKNavigationAction,
            decision_handler: &DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            decision_handler.call((navigation_policy(navigation_action),));
        }
    }

    unsafe impl WKUIDelegate for NavigationDelegate {
        /// Pages asking for a new window (`target="_blank"`, `window.open`) get the URL opened
        /// in the default browser instead.
        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn create_web_view(
            &self,
            _web_view: &WKWebView,
            _configuration: &WKWebViewConfiguration,
            navigation_action: &WKNavigationAction,
            _window_features: &WKWindowFeatures,
        ) -> Option<Retained<WKWebView>> {
            // SAFETY: reading properties of a live navigation action.
            if let Some(url) = unsafe { navigation_action.request() }.URL() {
                open_externally(&url);
            }
            None
        }
    }
);

impl NavigationDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's `init` signature is correct.
        unsafe { msg_send![super(this), init] }
    }
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
