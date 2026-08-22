use std::collections::HashMap;

use eframe::egui;

use floatdea::data::attachment::{ext_from_media_type, AttachmentStore, ImageError};
use floatdea::data::{AttachmentId, ExternalFileId, ExternalFileRef, ImageAttachment};

/// Identity of the file whose decoded bytes back an image on screen.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum ImageBytesKey {
    /// A managed workspace attachment.
    Attachment(AttachmentId),
    /// An external (over-limit) file referenced by absolute path.
    External(ExternalFileId),
}

/// Caches the raw file bytes of images so every frame can build a cheap
/// `egui::ImageSource::Bytes` without re-reading from disk. Texture decoding
/// itself is handled and cached by egui's image loader.
#[derive(Default)]
pub(super) struct ImageBytesCache {
    cache: HashMap<ImageBytesKey, egui::load::Bytes>,
}

impl ImageBytesCache {
    pub(super) fn get(&self, key: &ImageBytesKey) -> Option<egui::load::Bytes> {
        self.cache.get(key).cloned()
    }

    pub(super) fn insert(&mut self, key: ImageBytesKey, bytes: egui::load::Bytes) {
        self.cache.insert(key, bytes);
    }

    pub(super) fn remove(&mut self, key: &ImageBytesKey) {
        self.cache.remove(key);
    }
}

/// A stable URI for egui's image loader, which caches decoded textures by URI.
pub(super) fn attachment_uri(image: &ImageAttachment) -> String {
    format!(
        "attachment://{}.{}",
        image.id.as_str(),
        ext_from_media_type(&image.media_type)
    )
}

/// A stable URI for an external (over-limit) image file.
pub(super) fn external_image_uri(file: &ExternalFileRef) -> String {
    let ext = image_ext_from_path(&file.path);
    format!("external-image://{}.{}", file.id.as_str(), ext)
}

/// Reads the raw bytes of a managed image attachment.
pub(super) fn load_attachment_bytes(
    store: &AttachmentStore,
    image: &ImageAttachment,
) -> Result<Vec<u8>, ImageError> {
    store.read(image)
}

/// Reads the raw bytes of an external image file.
pub(super) fn load_external_bytes(file: &ExternalFileRef) -> std::io::Result<Vec<u8>> {
    std::fs::read(&file.path)
}

fn image_ext_from_path(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("jpeg") | Some("jpg") => "jpg",
        Some("png") => "png",
        _ => "jpg",
    }
}

/// Default canvas display size for a new image: 240 logical points wide, height
/// proportional to the pixel aspect ratio, clamped into the 80×60 .. 480×360
/// bounding box.
pub(super) const IMAGE_DEFAULT_WIDTH: f32 = 240.0;
pub(super) const IMAGE_MIN_SIZE: [f32; 2] = [80.0, 60.0];
pub(super) const IMAGE_MAX_SIZE: [f32; 2] = [480.0, 360.0];

pub(super) fn default_image_size(pixel_size: [u32; 2]) -> [f32; 2] {
    let w = pixel_size[0].max(1) as f32;
    let h = pixel_size[1].max(1) as f32;
    let mut size = [IMAGE_DEFAULT_WIDTH, IMAGE_DEFAULT_WIDTH * h / w];
    // Fit into the max bounding box (scale down only, preserving aspect).
    size = fit_into(size, IMAGE_MAX_SIZE);
    // Grow toward the min bounding box, but never beyond the max box, so
    // extreme aspect ratios can never escape the max bounds.
    let grow = (IMAGE_MIN_SIZE[0] / size[0]).max(IMAGE_MIN_SIZE[1] / size[1]);
    if grow > 1.0 {
        let grown = [size[0] * grow, size[1] * grow];
        size = fit_into(grown, IMAGE_MAX_SIZE);
    }
    size
}

/// Scales `size` down (never up) so it fits inside `bounds`, preserving aspect.
fn fit_into(size: [f32; 2], bounds: [f32; 2]) -> [f32; 2] {
    let scale = (bounds[0] / size[0]).min(bounds[1] / size[1]).min(1.0);
    [size[0] * scale, size[1] * scale]
}

/// An aspect-ratio-preserving resize driven by a target width: the height
/// follows the pixel ratio, and the result is clamped to the min/max bounding
/// box (preferring the max box for extreme ratios).
pub(super) fn scale_preserving_size(
    pixel_size: [u32; 2],
    target: egui::Vec2,
) -> [f32; 2] {
    let w = pixel_size[0].max(1) as f32;
    let h = pixel_size[1].max(1) as f32;
    let ratio = h / w;
    let width = target.x.max(IMAGE_MIN_SIZE[0]).clamp(IMAGE_MIN_SIZE[0], IMAGE_MAX_SIZE[0]);
    let mut new = [width, width * ratio];
    if new[1] > IMAGE_MAX_SIZE[1] {
        new[1] = IMAGE_MAX_SIZE[1];
        new[0] = new[1] / ratio;
        new[0] = new[0].clamp(IMAGE_MIN_SIZE[0], IMAGE_MAX_SIZE[0]);
        new[1] = new[0] * ratio;
        // Extreme ratios: keep the max box as the hard ceiling.
        new = fit_into(new, IMAGE_MAX_SIZE);
    }
    new
}

/// Builds a bytes image source for a managed attachment.
pub(super) fn attachment_source(
    image: &ImageAttachment,
    bytes: egui::load::Bytes,
) -> egui::ImageSource<'static> {
    egui::ImageSource::Bytes {
        uri: attachment_uri(image).into(),
        bytes,
    }
}

/// Builds a bytes image source for an external image file.
pub(super) fn external_source(file: &ExternalFileRef, bytes: egui::load::Bytes) -> egui::ImageSource<'static> {
    egui::ImageSource::Bytes {
        uri: external_image_uri(file).into(),
        bytes,
    }
}

/// The full-screen / in-app image viewer.
pub(super) struct ImageViewer {
    pub key: ImageBytesKey,
    pub title: String,
    pub uri: String,
    pub bytes: egui::load::Bytes,
    /// Texture size in pixels, once known.
    pub pixel_size: Option<[u32; 2]>,
    /// File size in bytes, when known.
    pub byte_len: Option<u64>,
    /// Absolute path for "Open Original in System Viewer".
    pub original_path: Option<String>,
    /// `Some` = fixed zoom scale; `None` = fit to window every frame.
    pub fixed_scale: Option<f32>,
    /// Keeps a texture handle so `pixel_size` can be discovered asynchronously.
    loaded_size: Option<egui::Vec2>,
}

pub(super) enum ViewerAction {
    None,
    /// Open the original file with the system's default viewer.
    OpenOriginal(String),
    Close,
}

impl ImageViewer {
    pub(super) fn new(
        key: ImageBytesKey,
        title: String,
        uri: String,
        bytes: egui::load::Bytes,
        pixel_size: Option<[u32; 2]>,
        byte_len: Option<u64>,
        original_path: Option<String>,
    ) -> Self {
        Self {
            key,
            title,
            uri,
            bytes,
            pixel_size,
            byte_len,
            original_path,
            fixed_scale: None,
            loaded_size: None,
        }
    }
}

/// Renders the image viewer as its own native OS window (a dedicated viewport,
/// matching how snippets open in separate windows). Returns the action the
/// caller must apply.
pub(super) fn render_viewer(ui: &mut egui::Ui, viewer: &mut ImageViewer) -> ViewerAction {
    let title = viewer.title.clone();
    ui.show_viewport_immediate(
        egui::ViewportId::from_hash_of(("image-viewer", viewer.key.key_label())),
        egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([560.0, 420.0])
            .with_min_inner_size([280.0, 200.0]),
        |child_ui, _| {
            let mut local_action = ViewerAction::None;
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::new()
                        .inner_margin(egui::Margin::same(8))
                        .fill(child_ui.visuals().panel_fill),
                )
                .show(child_ui, |ui| {
                    let ctx = ui.ctx();

                    // Discover the natural texture size asynchronously.
                    if viewer.loaded_size.is_none() {
                        let source = egui::ImageSource::Bytes {
                            uri: viewer.uri.clone().into(),
                            bytes: viewer.bytes.clone(),
                        };
                        if let Ok(poll) = source.load(
                            ctx,
                            egui::TextureOptions::LINEAR,
                            egui::load::SizeHint::Scale(1.0.into()),
                        ) {
                            if let egui::load::TexturePoll::Ready { texture, .. } = poll {
                                let size = texture.size;
                                viewer.loaded_size = Some(size);
                                if viewer.pixel_size.is_none() {
                                    viewer.pixel_size =
                                        Some([size.x.round() as u32, size.y.round() as u32]);
                                }
                            } else {
                                ctx.request_repaint();
                            }
                        }
                    }

                    let natural = viewer
                        .loaded_size
                        .map(|s| egui::Vec2::new(s.x, s.y))
                        .or_else(|| {
                            viewer
                                .pixel_size
                                .map(|[w, h]| egui::Vec2::new(w as f32, h as f32))
                        });

                    // No zoom/scroll in this phase. Default fits the image into
                    // the available window area (downscaling only); the context
                    // menu can switch to 1:1 (natural size).
                    let avail = ui.available_size();
                    let display_size = match (natural, viewer.fixed_scale) {
                        (Some(natural), Some(1.0)) => natural.round(),
                        (Some(natural), _) => {
                            let scale = (avail.x / natural.x).min(avail.y / natural.y).min(1.0);
                            (natural * scale).round()
                        }
                        (None, _) => avail,
                    };

                    let image = egui::Image::new(egui::ImageSource::Bytes {
                        uri: viewer.uri.clone().into(),
                        bytes: viewer.bytes.clone(),
                    })
                    .texture_options(egui::TextureOptions::LINEAR)
                    .fit_to_exact_size(display_size);

                    // Fit the image into the available space. The actions that
                    // used to live in a top toolbar are now a right-click menu.
                    let (rect, response) = ui.allocate_exact_size(display_size, egui::Sense::click());
                    if ui.is_rect_visible(rect) {
                        image.paint_at(ui, rect);
                    }

                    // Right-click menu: Fit to Window / 1:1 / Open Original.
                    response.context_menu(|ui| {
                        if ui.button("Fit to Window").clicked() {
                            viewer.fixed_scale = None;
                            ui.close();
                        }
                        if ui.button("1:1").clicked() {
                            viewer.fixed_scale = Some(1.0);
                            ui.close();
                        }
                        if let Some(path) = viewer.original_path.clone() {
                            ui.separator();
                            if ui.button("Open Original in System Viewer").clicked() {
                                local_action = ViewerAction::OpenOriginal(path);
                                ui.close();
                            }
                        }
                    });

                    // Metadata footer.
                    let mut meta = Vec::new();
                    if let Some([w, h]) = viewer.pixel_size {
                        meta.push(format!("{w}×{h} px"));
                    }
                    if let Some(len) = viewer.byte_len {
                        meta.push(format!("{:.1} KB", len as f64 / 1024.0));
                    }
                    if !meta.is_empty() {
                        ui.separator();
                        ui.label(egui::RichText::new(meta.join(" · ")).weak());
                    }
                });
            if child_ui.input(|input| input.viewport().close_requested()) {
                local_action = ViewerAction::Close;
            }
            local_action
        },
    )
}

impl ImageBytesKey {
    fn key_label(&self) -> String {
        match self {
            ImageBytesKey::Attachment(id) => id.as_str().to_owned(),
            ImageBytesKey::External(id) => id.as_str().to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_size_keeps_aspect_and_bounds() {
        // Square 1000x1000 -> 240x240.
        let s = default_image_size([1000, 1000]);
        assert!((s[0] - 240.0).abs() < 0.01);
        assert!((s[1] - 240.0).abs() < 0.01);

        // Very wide -> width capped by max box, aspect preserved.
        let s = default_image_size([4000, 500]);
        assert!(s[0] <= IMAGE_MAX_SIZE[0] + 0.01);
        assert!(s[1] >= IMAGE_MIN_SIZE[1] - 0.01);
        assert!((s[1] / s[0] - 0.125).abs() < 0.01);

        // Very tall -> height capped by max box, aspect preserved.
        let s = default_image_size([500, 4000]);
        assert!(s[1] <= IMAGE_MAX_SIZE[1] + 0.01);
        assert!(s[0] >= IMAGE_MIN_SIZE[0] - 0.01 || (s[1] / s[0] - 8.0).abs() < 0.01);
        assert!((s[1] / s[0] - 8.0).abs() < 0.01);

        // Huge -> capped by max bounding box.
        let s = default_image_size([10000, 10000]);
        assert!(s[0] <= IMAGE_MAX_SIZE[0] + 0.01 && s[1] <= IMAGE_MAX_SIZE[1] + 0.01);
        assert!((s[0] - s[1]).abs() < 0.01);
    }

    #[test]
    fn scale_preserves_aspect_ratio() {
        let pixel = [400, 200];
        let sized = scale_preserving_size(pixel, egui::vec2(300.0, 1000.0));
        let ratio = pixel[1] as f32 / pixel[0] as f32;
        assert!((sized[1] / sized[0] - ratio).abs() < 0.01);
    }
}
