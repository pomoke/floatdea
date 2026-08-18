//! Content extraction from external files for AI source consumption.
//!
//! Text-based files (`.md`, `.txt`, etc.) are read directly as UTF-8. PDF
//! files are extracted via `pdf-extract`. All content is capped at
//! [`MAX_EXTRACT_CHARS`] to bound memory use.

use std::path::Path;

/// Maximum number of characters to extract from a single external file.
const MAX_EXTRACT_CHARS: usize = 100 * 1024;

/// File extensions treated as plain text (read directly as UTF-8).
const TEXT_EXTENSIONS: &[&str] = &[
    "md", "txt", "json", "csv", "toml", "yaml", "yml", "xml", "html", "htm",
    "css", "js", "ts", "rs", "py", "rb", "sh", "bash", "zsh", "fish",
    "ini", "cfg", "conf", "log", "sql", "r", "java", "cpp", "c", "h", "hpp",
    "go", "php", "pl", "lua", "tex", "bib", "org", "rst",
];

/// Returns `true` if `path` has a text-file extension.
fn is_text_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| TEXT_EXTENSIONS.contains(&ext))
        .unwrap_or(false)
}

/// Returns `true` if `path` has a PDF extension.
fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
}

/// Extracts text content from a file at `path`.
///
/// Returns `None` when the file is missing, not readable, or its format is
/// unsupported. Content is capped at [`MAX_EXTRACT_CHARS`] characters.
pub fn extract_text_from_file(path: &str) -> Option<String> {
    let path_obj = Path::new(path);

    if is_text_file(path_obj) {
        read_text_file(path)
    } else if is_pdf(path_obj) {
        extract_pdf_text(path)
    } else {
        // Unknown extension: try plain UTF-8 as a fallback.
        read_text_file(path)
    }
}

/// Extracts text from a PDF file. Runs synchronously; for large PDFs this
/// may block the calling thread for a few hundred milliseconds.
fn extract_pdf_text(path: &str) -> Option<String> {
    // Try reading the file ourselves and using extract_text_from_mem, which
    // avoids any path-resolution issues in the pdf-extract crate.
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            log::warn!("cannot read PDF file {path}: {e}");
            return None;
        }
    };
    match pdf_extract::extract_text_from_mem(&bytes) {
        Ok(text) => {
            if text.is_empty() {
                log::warn!("PDF text extraction returned empty (no text layer?): {path}");
            }
            Some(truncate(&text))
        }
        Err(e) => {
            log::warn!("failed to extract text from PDF {path}: {e}");
            None
        }
    }
}

/// Tries to read a text file. Returns `None` with a warning on failure.
fn read_text_file(path: &str) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(content) => Some(truncate(&content)),
        Err(e) => {
            log::warn!("cannot read text file {path}: {e}");
            None
        }
    }
}

fn truncate(s: &str) -> String {
    s.chars().take(MAX_EXTRACT_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_file_is_detected_by_extension() {
        assert!(is_text_file(Path::new("note.md")));
        assert!(is_text_file(Path::new("data.json")));
        assert!(is_text_file(Path::new("script.rs")));
        assert!(!is_text_file(Path::new("image.png")));
        assert!(!is_text_file(Path::new("archive.zip")));
    }

    #[test]
    fn pdf_is_detected_by_extension() {
        assert!(is_pdf(Path::new("doc.pdf")));
        assert!(is_pdf(Path::new("DOC.PDF")));
        assert!(!is_pdf(Path::new("doc.txt")));
    }

    #[test]
    fn missing_file_returns_none() {
        assert!(extract_text_from_file("/tmp/floatdea-nonexistent-xyz.md").is_none());
    }

    #[test]
    fn text_file_is_read_and_truncated() {
        let dir = std::env::temp_dir().join("floatdea-extract-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("hello.txt");
        std::fs::write(&path, "Hello, world!").unwrap();
        let result = extract_text_from_file(path.to_str().unwrap());
        assert_eq!(result, Some("Hello, world!".to_owned()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn truncation_caps_at_max_chars() {
        let long = "a".repeat(MAX_EXTRACT_CHARS + 100);
        assert_eq!(truncate(&long).len(), MAX_EXTRACT_CHARS);
    }

    #[test]
    fn unknown_extension_tries_utf8_fallback() {
        let dir = std::env::temp_dir().join("floatdea-extract-test-unknown");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("data.xyz");
        std::fs::write(&path, "fallback content").unwrap();
        let result = extract_text_from_file(path.to_str().unwrap());
        assert_eq!(result, Some("fallback content".to_owned()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pdf_file_is_detected_and_attempts_extraction() {
        // Verify that a .pdf file is detected and pdf_extract is called.
        // We use a non-existent file to test that the function returns None
        // (the pdf_extract error is logged via `log::warn!`).
        let dir = std::env::temp_dir().join("floatdea-extract-pdf-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test.pdf");
        // Write a tiny invalid PDF blob so pdf_extract attempts parsing.
        std::fs::write(&path, b"not a real pdf").unwrap();
        let result = extract_text_from_file(path.to_str().unwrap());
        // Should be None because pdf_extract will fail to parse the file.
        assert!(result.is_none(), "invalid PDF content should return None");
        let _ = std::fs::remove_dir_all(&dir);
    }
}