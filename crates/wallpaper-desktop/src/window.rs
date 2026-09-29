//! Native window around the local library page.
//!
//! [`open_library_window`] is the only function that creates a tao/wry
//! webview. Tests call the server and settings functions instead, including
//! when no display is available.

use std::env;

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

/// `DISPLAY` or `WAYLAND_DISPLAY` is set to a non-empty value.
pub fn display_available() -> bool {
    nonempty("WAYLAND_DISPLAY") || nonempty("DISPLAY")
}

fn nonempty(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// Open a system webview at `url` and run until the window closes.
///
/// On Linux the webview is built inside tao's GTK box, so the same window
/// works on X11 and Wayland. This blocks the calling thread.
pub fn open_library_window(url: &str) -> Result<(), String> {
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("Wallpapers")
        .with_inner_size(LogicalSize::new(1120.0, 780.0))
        .build(&event_loop)
        .map_err(|error| error.to_string())?;

    let builder = WebViewBuilder::new().with_url(url);

    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window
            .default_vbox()
            .ok_or_else(|| "tao window has no gtk box".to_string())?;
        builder.build_gtk(vbox).map_err(|error| error.to_string())?
    };
    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window).map_err(|error| error.to_string())?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        let _keep_window = &window;
        let _keep_webview = &webview;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
        }
    });
}
