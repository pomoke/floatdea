use std::{
    fs, io,
    path::{Path, PathBuf},
};

use super::{AttachmentId, ImageAttachment};

// ── Resource limits ────────────────────────────────────────────────────────

/// Maximum file size (in bytes) for a managed inline image. Files larger than
/// this are stored as external references (absolute path, no copy).
pub const MAX_INLINE_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// Maximum image width in pixels.
pub const MAX_IMAGE_WIDTH: u32 = 8192;

/// Maximum image height in pixels.
pub const MAX_IMAGE_HEIGHT: u32 = 8192;

/// Maximum total pixel count (width × height).
pub const MAX_IMAGE_PIXELS: u64 = 64_000_000;

/// Maximum number of concurrent image decode tasks.
pub const MAX_CONCURRENT_DECODES: usize = 2;

// ── Error types ────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ImageError {
    Io(io::Error),
    FileTooLarge { bytes: u64, max: u64 },
    UnsupportedFormat(String),
    DimensionsTooLarge { width: u32, height: u32 },
    TooManyPixels { pixels: u64, max: u64 },
    DecodeFailed(String),
    InvalidPath(String),
    // The image file exceeds the inline limit and should be handled as an
    // external reference instead.
    OverInlineLimit(PathBuf),
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageError::Io(e) => write!(f, "I/O error: {e}"),
            ImageError::FileTooLarge { bytes, max } => {
                write!(f, "file size {bytes} exceeds maximum {max}")
            }
            ImageError::UnsupportedFormat(fmt) => write!(f, "unsupported image format: {fmt}"),
            ImageError::DimensionsTooLarge { width, height } => {
                write!(f, "image dimensions {width}×{height} exceed limit")
            }
            ImageError::TooManyPixels { pixels, max } => {
                write!(f, "pixel count {pixels} exceeds maximum {max}")
            }
            ImageError::DecodeFailed(msg) => write!(f, "image decode failed: {msg}"),
            ImageError::InvalidPath(msg) => write!(f, "invalid path: {msg}"),
            ImageError::OverInlineLimit(path) => {
                write!(f, "file exceeds inline limit, use external reference: {}", path.display())
            }
        }
    }
}

impl std::error::Error for ImageError {}

impl From<io::Error> for ImageError {
    fn from(e: io::Error) -> Self {
        ImageError::Io(e)
    }
}

// ── PreparedImage ──────────────────────────────────────────────────────────

/// An image that has been validated, decoded, and copied to the workspace
/// temporary directory, but not yet committed to the workspace.
pub struct PreparedImage {
    pub image: ImageAttachment,
    pub temporary_path: PathBuf,
}

// ── AttachmentStore ────────────────────────────────────────────────────────

/// Manages the lifecycle of image attachments in a workspace.
///
/// Files are stored in `attachments/` under the workspace root. Each image
/// gets a stable `AttachmentId` and a content-addressable file name.
#[derive(Clone)]
pub struct AttachmentStore {
    root: PathBuf,
    attachments_dir: PathBuf,
    temp_dir: PathBuf,
}

impl AttachmentStore {
    /// Opens the attachment store for the workspace at `root`. Creates the
    /// `attachments/` directory and its `.tmp/` subdirectory if missing, and
    /// cleans up any stale temporary files from a previous crash.
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        let attachments_dir = root.join("attachments");
        let temp_dir = attachments_dir.join(".tmp");

        fs::create_dir_all(&attachments_dir)?;
        fs::create_dir_all(&temp_dir)?;

        // Clean up stale temporary files.
        if let Ok(entries) = fs::read_dir(&temp_dir) {
            for entry in entries.flatten() {
                let _ = fs::remove_file(entry.path());
            }
        }

        Ok(Self {
            root,
            attachments_dir,
            temp_dir,
        })
    }

    /// Returns the path to the `attachments/` directory.
    pub fn attachments_dir(&self) -> &Path {
        &self.attachments_dir
    }

    /// Returns the absolute path of a committed image file.
    pub fn image_path(&self, image: &ImageAttachment) -> PathBuf {
        self.root.join(&image.relative_path)
    }

    /// Validates, decodes, and copies the source file into the workspace
    /// temporary directory. Returns a `PreparedImage` that can be inspected
    /// before committing.
    ///
    /// Returns `ImageError::OverInlineLimit` when the source file exceeds
    /// `MAX_INLINE_IMAGE_BYTES`; the caller should fall back to creating an
    /// external reference instead.
    pub fn prepare_image(&self, source: &Path) -> Result<PreparedImage, ImageError> {
        let metadata = fs::metadata(source).map_err(ImageError::Io)?;
        let byte_len = metadata.len();

        if byte_len > MAX_INLINE_IMAGE_BYTES {
            return Err(ImageError::OverInlineLimit(source.to_owned()));
        }

        let bytes = fs::read(source)?;
        let (format, media_type, ext) = detect_format(&bytes)?;
        let pixel_size = decode_dimensions(&bytes, format)?;
        let content_hash = hash_content(&bytes);

        let id = AttachmentId::new();
        let relative_path = format!("attachments/{}.{}", id.as_str(), ext);
        let title = source
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("untitled")
            .to_owned();

        let image = ImageAttachment {
            id: id.clone(),
            relative_path: relative_path.clone(),
            title,
            media_type: media_type.to_owned(),
            byte_len,
            content_hash,
            pixel_size,
            original_name: None,
        };

        let temporary_path = self.temp_dir.join(format!("{}.{}", id.as_str(), ext));
        fs::write(&temporary_path, &bytes)?;

        Ok(PreparedImage {
            image,
            temporary_path,
        })
    }

    /// Commits a prepared image: moves the temporary file into `attachments/`
    /// and registers it in the workspace. Returns the committed `ImageAttachment`.
    pub fn commit(&self, prepared: PreparedImage) -> Result<ImageAttachment, ImageError> {
        let target = self.attachments_dir.join(format!(
            "{}.{}",
            prepared.image.id.as_str(),
            ext_from_media_type(&prepared.image.media_type)
        ));

        if target.exists() {
            return Err(ImageError::Io(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("attachment file already exists: {}", target.display()),
            )));
        }

        fs::rename(&prepared.temporary_path, &target)?;
        Ok(prepared.image)
    }

    /// Reads the full file content of a committed image attachment.
    pub fn read(&self, image: &ImageAttachment) -> Result<Vec<u8>, ImageError> {
        let path = self.image_path(image);
        Ok(fs::read(path)?)
    }

    /// Verifies that the image file exists on disk and its content hash matches.
    pub fn verify(&self, image: &ImageAttachment) -> Result<(), ImageError> {
        let path = self.image_path(image);
        if !path.exists() {
            return Err(ImageError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!("image file not found: {}", path.display()),
            )));
        }
        let bytes = fs::read(&path)?;
        let actual_hash = hash_content(&bytes);
        if actual_hash != image.content_hash {
            return Err(ImageError::DecodeFailed(format!(
                "content hash mismatch for {}",
                image.id.as_str()
            )));
        }
        Ok(())
    }

    /// Moves the image file to the workspace trash directory. Does not remove
    /// the workspace registration.
    pub fn move_to_trash(&self, image: &ImageAttachment) -> Result<(), ImageError> {
        let source = self.image_path(image);
        let trash = self.root.join(".floatdea/trash/attachments");
        fs::create_dir_all(&trash)?;
        let target = trash.join(format!("{}.{}", image.id.as_str(), ext_from_media_type(&image.media_type)));
        fs::rename(&source, &target)?;
        Ok(())
    }
}

// ── Format detection ───────────────────────────────────────────────────────

fn detect_format(bytes: &[u8]) -> Result<(&'static str, &'static str, &'static str), ImageError> {
    if bytes.len() < 4 {
        return Err(ImageError::UnsupportedFormat("file too small".into()));
    }

    // JPEG: starts with FF D8 FF
    if bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        return Ok(("jpeg", "image/jpeg", "jpg"));
    }

    // PNG: starts with 89 50 4E 47 0D 0A 1A 0A
    if bytes.len() >= 8
        && bytes[0] == 0x89
        && bytes[1] == 0x50
        && bytes[2] == 0x4E
        && bytes[3] == 0x47
        && bytes[4] == 0x0D
        && bytes[5] == 0x0A
        && bytes[6] == 0x1A
        && bytes[7] == 0x0A
    {
        return Ok(("png", "image/png", "png"));
    }

    Err(ImageError::UnsupportedFormat(
        "unsupported magic bytes (only JPEG and PNG are supported)".into(),
    ))
}

fn decode_dimensions(bytes: &[u8], _format: &str) -> Result<[u32; 2], ImageError> {
    let img = image::load_from_memory(bytes).map_err(|e| {
        ImageError::DecodeFailed(format!("{e}"))
    })?;

    let (width, height) = (img.width(), img.height());

    if width > MAX_IMAGE_WIDTH || height > MAX_IMAGE_HEIGHT {
        return Err(ImageError::DimensionsTooLarge { width, height });
    }

    let pixels = (width as u64).saturating_mul(height as u64);
    if pixels > MAX_IMAGE_PIXELS {
        return Err(ImageError::TooManyPixels { pixels, max: MAX_IMAGE_PIXELS });
    }

    Ok([width, height])
}

fn hash_content(bytes: &[u8]) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    let result = hasher.finalize();
    result.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Maps a MIME type to the canonical attachment file extension.
pub fn ext_from_media_type(media_type: &str) -> &'static str {
    match media_type {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        _ => "bin",
    }
}

/// Builds the workspace-relative path for a managed image file
/// (`attachments/{id}.{ext}`), given the canonical extension.
pub fn attachment_relative_path(id: &AttachmentId, ext: &str) -> String {
    format!("attachments/{}.{ext}", id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_jpeg_from_magic_bytes() {
        let header = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46];
        let (name, mime, ext) = detect_format(&header).unwrap();
        assert_eq!(name, "jpeg");
        assert_eq!(mime, "image/jpeg");
        assert_eq!(ext, "jpg");
    }

    #[test]
    fn detect_png_from_magic_bytes() {
        let header = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00];
        let (name, mime, ext) = detect_format(&header).unwrap();
        assert_eq!(name, "png");
        assert_eq!(mime, "image/png");
        assert_eq!(ext, "png");
    }

    #[test]
    fn reject_unsupported_format() {
        let header = [0x00, 0x00, 0x00, 0x00];
        assert!(detect_format(&header).is_err());
    }

    #[test]
    fn reject_empty_file() {
        assert!(detect_format(&[]).is_err());
    }

    #[test]
    fn test_image_limits_constants_are_sane() {
        assert_eq!(MAX_INLINE_IMAGE_BYTES, 20 * 1024 * 1024);
        assert!(MAX_IMAGE_WIDTH > 0);
        assert!(MAX_IMAGE_HEIGHT > 0);
        assert!(MAX_IMAGE_PIXELS > 0);
    }
}