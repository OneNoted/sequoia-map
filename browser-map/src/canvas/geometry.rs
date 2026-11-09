//! Where the canvas is and how big, as its `ResizeObserver` reports it, so neither frames nor
//! pointer events read layout. A layout read forces the browser to finish any pending style
//! and layout work on the spot, in the middle of an event or a frame callback.

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{
    Element, HtmlCanvasElement, ResizeObserver, ResizeObserverBoxOptions, ResizeObserverEntry,
    ResizeObserverOptions, ResizeObserverSize,
};

/// The canvas's layout box.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct CanvasBox {
    /// CSS pixels, fractional where the layout is.
    pub(crate) css: (f64, f64),
    /// Exact device pixels, where the browser reports them.
    pub(crate) device: Option<(u32, u32)>,
    /// The top-left corner in client coordinates; `None` once it may have moved.
    pub(crate) origin: Option<(f64, f64)>,
}

impl CanvasBox {
    /// The backing store's size and its device pixels per CSS pixel: the exact device pixels
    /// where known, at least one backing pixel per CSS pixel, and fewer per CSS pixel rather
    /// than a side longer than `max_side`.
    pub(crate) fn backing(&self, device_pixel_ratio: f64, max_side: f64) -> (u32, u32, f64) {
        let (css_width, css_height) = (self.css.0.max(1.0), self.css.1.max(1.0));
        if let Some((width, height)) = self.device
            && f64::from(width) >= css_width.floor()
            && f64::from(height) >= css_height.floor()
            && f64::from(width.max(height)) <= max_side
        {
            return (width, height, f64::from(width) / css_width);
        }
        let dpr = device_pixel_ratio
            .max(1.0)
            .min(max_side / css_width)
            .min(max_side / css_height);
        (
            (css_width * dpr).round().max(1.0) as u32,
            (css_height * dpr).round().max(1.0) as u32,
            dpr,
        )
    }
}

/// Reports the canvas's [`CanvasBox`] to `changed` whenever it changes, while it lives.
pub(crate) struct CanvasObserver {
    observer: ResizeObserver,
    _callback: Closure<dyn FnMut(js_sys::Array)>,
}

impl CanvasObserver {
    pub(crate) fn new(
        canvas: &HtmlCanvasElement,
        changed: impl Fn(CanvasBox) + 'static,
    ) -> Option<Self> {
        let callback = Closure::<dyn FnMut(js_sys::Array)>::new(move |entries: js_sys::Array| {
            let Some(entry) = entries
                .iter()
                .last()
                .and_then(|entry| entry.dyn_into::<ResizeObserverEntry>().ok())
            else {
                return;
            };
            let rect = entry.content_rect();
            // Browsers without device-pixel boxes (Safari) have no such property at all, and
            // web-sys's getter would index `undefined`.
            let device = js_sys::Reflect::get(&entry, &"devicePixelContentBoxSize".into())
                .ok()
                .and_then(|sizes| sizes.dyn_into::<js_sys::Array>().ok())
                .and_then(|sizes| sizes.get(0).dyn_into::<ResizeObserverSize>().ok())
                .map(|size| (size.inline_size() as u32, size.block_size() as u32));
            // Observers run after layout, so reading the position here costs nothing extra.
            let client = entry.target().get_bounding_client_rect();
            changed(CanvasBox {
                css: (rect.width(), rect.height()),
                device,
                origin: Some((client.left(), client.top())),
            });
        });
        let observer = ResizeObserver::new(callback.as_ref().unchecked_ref()).ok()?;
        let element: &Element = canvas.as_ref();
        let options = ResizeObserverOptions::new();
        options.set_box(ResizeObserverBoxOptions::DevicePixelContentBox);
        // Browsers without device-pixel boxes throw on the option; the content box still
        // gives the CSS size. web-sys's `observe_with_options` cannot catch that throw.
        let with_device_pixels = js_sys::Reflect::get(&observer, &"observe".into())
            .ok()
            .and_then(|observe| observe.dyn_into::<js_sys::Function>().ok())
            .is_some_and(|observe| observe.call2(&observer, element, &options).is_ok());
        if !with_device_pixels {
            observer.observe(element);
        }
        Some(Self {
            observer,
            _callback: callback,
        })
    }
}

impl Drop for CanvasObserver {
    fn drop(&mut self) {
        self.observer.disconnect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sized(css: (f64, f64), device: Option<(u32, u32)>) -> CanvasBox {
        CanvasBox {
            css,
            device,
            origin: None,
        }
    }

    #[test]
    fn exact_device_pixels_size_the_backing_store() {
        assert_eq!(
            sized((412.5, 915.0), Some((1083, 2402))).backing(2.625, 8192.0),
            (1083, 2402, 1083.0 / 412.5)
        );
    }

    #[test]
    fn without_device_pixels_the_ratio_sizes_it() {
        assert_eq!(
            sized((400.0, 300.0), None).backing(1.5, 8192.0),
            (600, 450, 1.5)
        );
    }

    #[test]
    fn a_zoomed_out_page_keeps_a_backing_pixel_per_css_pixel() {
        assert_eq!(
            sized((1600.0, 900.0), Some((1200, 675))).backing(0.75, 8192.0),
            (1600, 900, 1.0)
        );
    }

    #[test]
    fn a_surface_the_device_cannot_hold_gets_fewer_pixels_per_css_pixel() {
        let (width, height, dpr) = sized((3000.0, 1000.0), Some((9000, 3000))).backing(3.0, 4096.0);
        assert_eq!((width, height), (4096, 1365));
        assert!((dpr - 4096.0 / 3000.0).abs() < 1e-9);
    }
}
