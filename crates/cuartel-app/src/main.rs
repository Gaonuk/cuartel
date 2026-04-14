mod app;
mod assets;
mod sidebar;
mod theme;
mod workspace;

use app::CuartelApp;
use assets::Assets;
use gpui::*;

fn main() {
    env_logger::init();

    Application::new()
        .with_assets(Assets)
        .run(|cx: &mut App| {
            if let Err(e) = Assets.load_fonts(cx) {
                log::error!("failed to load fonts: {e}");
            }

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
                |_, cx| cx.new(|cx| CuartelApp::new(cx)),
            )
            .unwrap();
        });
}
