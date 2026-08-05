mod app;

use std::{collections::BTreeMap, fs, path::PathBuf, sync::Arc};

use app::HomePage;
use system_fonts::{FontPreset, FontStyle, FoundFont, FoundFontSource};

fn read_font(font: FoundFont) -> Option<(String, Arc<egui::FontData>)> {
    let bytes = match font.source {
        FoundFontSource::Path(path) => match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                log::warn!("failed to read system font {path:?}: {error}");
                return None;
            }
        },
        FoundFontSource::Bytes(bytes) => bytes.as_ref().to_vec(),
    };
    Some((font.key, Arc::new(egui::FontData::from_owned(bytes))))
}

fn load_family(
    presets: impl IntoIterator<Item = FontPreset>,
    font_data: &mut BTreeMap<String, Arc<egui::FontData>>,
) -> Vec<String> {
    system_fonts::find_from_presets(presets, FontStyle::Sans)
        .into_iter()
        .filter_map(read_font)
        .map(|(key, data)| {
            font_data.insert(key.clone(), data);
            key
        })
        .collect()
}

fn install_system_fonts(ctx: &egui::Context) {
    const NOTO_EMOJI: &str = "NotoEmoji-Regular";
    const EMOJI_ICON: &str = "emoji-icon-font";

    let mut fonts = egui::FontDefinitions::empty();
    let mut proportional = load_family(
        [
            FontPreset::Latin,
            FontPreset::SimplifiedChinese,
            FontPreset::TraditionalChinese,
            FontPreset::Japanese,
            FontPreset::Korean,
            FontPreset::Cyrillic,
        ],
        &mut fonts.font_data,
    );
    assert!(
        !proportional.is_empty(),
        "no usable proportional system font was found"
    );

    let mut monospace = load_family(
        [FontPreset::Custom(vec![
            "Cascadia Mono".to_owned(),
            "SF Mono".to_owned(),
            "Noto Sans Mono".to_owned(),
            "DejaVu Sans Mono".to_owned(),
            "Liberation Mono".to_owned(),
        ])],
        &mut fonts.font_data,
    );
    if monospace.is_empty() {
        monospace = proportional.clone();
    }

    fonts.font_data.insert(
        NOTO_EMOJI.to_owned(),
        Arc::new(
            egui::FontData::from_static(epaint_default_fonts::NOTO_EMOJI_REGULAR).tweak(
                egui::FontTweak {
                    scale: 0.81,
                    ..Default::default()
                },
            ),
        ),
    );
    fonts.font_data.insert(
        EMOJI_ICON.to_owned(),
        Arc::new(
            egui::FontData::from_static(epaint_default_fonts::EMOJI_ICON).tweak(egui::FontTweak {
                scale: 0.90,
                ..Default::default()
            }),
        ),
    );

    proportional.extend([NOTO_EMOJI.to_owned(), EMOJI_ICON.to_owned()]);
    monospace.extend([NOTO_EMOJI.to_owned(), EMOJI_ICON.to_owned()]);

    fonts
        .families
        .insert(egui::FontFamily::Proportional, proportional);
    fonts
        .families
        .insert(egui::FontFamily::Monospace, monospace);
    ctx.set_fonts(fonts);
}

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
            install_system_fonts(&cc.egui_ctx);
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(HomePage::new(workspace)))
        }),
    )
}
