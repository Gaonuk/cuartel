mod app;
mod assets;
mod permission_prompt;
mod session_host;
mod sidebar;
mod sidecar_host;
mod theme;
mod workspace;

use app::CuartelApp;
use assets::Assets;
use gpui::*;
use sidecar_host::{default_rivet_dir, SidecarHost};

const RIVET_PORT: u16 = 6420;

fn main() {
    env_logger::init();

    // Leak so the sidecar host lives for the entire app lifetime without
    // requiring a Send + 'static move into GPUI's run closure.
    let sidecar: &'static SidecarHost =
        Box::leak(Box::new(SidecarHost::spawn(default_rivet_dir(), RIVET_PORT)));

    Application::new()
        .with_assets(Assets)
        .run(|cx: &mut App| {
            if let Err(e) = Assets.load_fonts(cx) {
                log::error!("failed to load fonts: {e}");
            }

            let status_handle = sidecar.status();
            let client_handle = sidecar.client();
            let runtime_handle = sidecar.runtime_handle();
            let bounds = Bounds::centered(None, size(px(1200.0), px(800.0)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Cuartel".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |_, cx| {
                    cx.new(|cx| {
                        CuartelApp::new(
                            status_handle,
                            client_handle,
                            runtime_handle,
                            cx,
                        )
                    })
                },
            )
            .unwrap();
        });
}
