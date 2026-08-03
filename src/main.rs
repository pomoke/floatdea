mod app;

use std::path::PathBuf;

use app::HomePage;

fn default_workspace() -> PathBuf {
    std::env::var("HOME")
        .map(|home| PathBuf::from(home).join(".local/floatdea/workspace"))
        .unwrap_or_else(|_| PathBuf::from(".floatdea/workspace"))
}

fn main() -> eframe::Result {
    let workspace = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(default_workspace);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([640., 480.]),
        ..Default::default()
    };

    eframe::run_native(
        "floatdea",
        options,
        Box::new(|cc| {
            #[cfg(not(target_arch = "wasm32"))]
            cc.egui_ctx.set_embed_viewports(false);
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(HomePage::new(workspace)))
        }),
    )
}
