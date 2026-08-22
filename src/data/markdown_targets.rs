use super::{AttachmentId, EntityId};

/// A local target parsed from a Markdown reference `[...](target)` or
/// `![...](target)` inside a workspace snippet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalMarkdownTarget {
    /// A reference to another snippet entity (`{title}--{id}.md`).
    Snippet(EntityId),
    /// A reference to a managed image attachment (`attachments/{id}.{ext}`).
    Image(AttachmentId),
    /// A local path that could not be resolved to a known entity or image.
    UnresolvedLocal(String),
}

/// One Markdown inline image/link occurrence, with enough span information for
/// callers to rebuild or replace the text (e.g. reference counting).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedMarkdownTarget {
    /// Whether the occurrence is an image embed (`![alt](dest)`) rather than a
    /// plain link (`[text](dest)`).
    pub is_image: bool,
    /// The alt text / label between the brackets (raw, unescaped).
    pub label: String,
    /// The resolved target.
    pub target: LocalMarkdownTarget,
    /// The raw destination string (e.g. `attachments/{id}.png`).
    pub dest: String,
    /// Byte offset of the opening `[` (or `![`).
    pub start: usize,
    /// Byte offset just past the closing `)`.
    pub end: usize,
}

/// Parses every inline image/link in `content`, correctly skipping fenced code
/// blocks, inline code spans, and escaped characters. This is the single local
/// target resolver shared by preview, export closures, garbage collection and
/// import rewriting — callers must not hand-roll their own string scanning.
pub fn parse_local_markdown_targets(content: &str) -> Vec<ParsedMarkdownTarget> {
    let bytes = content.as_bytes();
    let mut targets = Vec::new();
    let mut i = 0;
    let mut in_fence = false;
    let mut fence_char = 0u8;

    while i < bytes.len() {
        // Backtick or tilde runs: an unbroken run of >= 3 toggles a fenced
        // block; shorter backtick runs are inline code spans (skipped, with
        // their matching closer).
        if bytes[i] == b'`' || bytes[i] == b'~' {
            let marker = bytes[i];
            let mut count = 0;
            while i + count < bytes.len() && bytes[i + count] == marker {
                count += 1;
            }
            if count >= 3 {
                if !in_fence {
                    in_fence = true;
                    fence_char = marker;
                } else if fence_char == marker {
                    in_fence = false;
                }
                i += count;
                continue;
            }
            if marker == b'`' && !in_fence {
                // Inline code span: skip until the closing run of equal length.
                let len = count;
                i += count;
                while i < bytes.len() {
                    let mut c = 0;
                    while i + c < bytes.len() && bytes[i + c] == b'`' {
                        c += 1;
                    }
                    i += c;
                    if c == len {
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            i += count;
            continue;
        }

        if !in_fence && bytes[i] == b'!' && bytes.get(i + 1) == Some(&b'[') && !is_escaped(bytes, i)
        {
            if let Some(parsed) = parse_one(bytes, content, i, true) {
                let end = parsed.1;
                targets.push(parsed.0);
                i = end;
                continue;
            }
        }
        if !in_fence && bytes[i] == b'[' && !is_escaped(bytes, i) {
            if let Some(parsed) = parse_one(bytes, content, i, false) {
                let end = parsed.1;
                targets.push(parsed.0);
                i = end;
                continue;
            }
        }
        i += 1;
    }
    targets
}

fn is_escaped(bytes: &[u8], i: usize) -> bool {
    let mut backslashes = 0;
    let mut j = i;
    while j > 0 && bytes[j - 1] == b'\\' {
        backslashes += 1;
        j -= 1;
    }
    backslashes % 2 == 1
}

/// Attempts to parse an inline link starting at `open` (the byte index of `[`
/// or the `!` of `![`). Returns `(target, end)` where `end` is just past `)`.
fn parse_one(
    bytes: &[u8],
    content: &str,
    open: usize,
    is_image: bool,
) -> Option<(ParsedMarkdownTarget, usize)> {
    let label_start = if is_image { open + 2 } else { open + 1 };
    let mut close = label_start;
    while close < bytes.len() && bytes[close] != b']' {
        close += 1;
    }
    if close >= bytes.len() || bytes.get(close + 1) != Some(&b'(') {
        return None;
    }
    let label = content[label_start..close].to_owned();

    // Balanced parentheses in the destination (rare but legal).
    let mut end = close + 2;
    let mut depth = 1usize;
    while end < bytes.len() && depth > 0 {
        match bytes[end] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {}
        }
        end += 1;
    }
    if depth != 0 {
        return None;
    }
    let dest = content[close + 2..end - 1].to_owned();

    let target = resolve_target(&dest);
    Some((
        ParsedMarkdownTarget {
            is_image,
            label,
            target,
            dest,
            start: open,
            end,
        },
        end,
    ))
}

/// Resolves a local destination string to a typed target. Scheme'd URLs and
/// `data:` URIs are never resolved locally.
pub fn resolve_target(dest: &str) -> LocalMarkdownTarget {
    if dest.contains("://") || dest.starts_with("data:") {
        return LocalMarkdownTarget::UnresolvedLocal(dest.to_owned());
    }
    if let Some(id) = parse_attachment_link(dest) {
        return LocalMarkdownTarget::Image(id);
    }
    if let Some(id) = parse_snippet_link(dest) {
        return LocalMarkdownTarget::Snippet(id);
    }
    LocalMarkdownTarget::UnresolvedLocal(dest.to_owned())
}

/// Parses a managed-image destination `attachments/{id}.{ext}` into its
/// `AttachmentId`. Only the trailing `attachments/{id}.{ext}` shape with a
/// plausible 20-char id is accepted.
pub fn parse_attachment_link(dest: &str) -> Option<AttachmentId> {
    let rest = dest.strip_prefix("attachments/")?;
    let dot = rest.rfind('.')?;
    let id = &rest[..dot];
    let ext = &rest[dot + 1..];
    let plausible = id.len() == 20
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && matches!(ext, "jpg" | "jpeg" | "png");
    plausible.then(|| AttachmentId::from_string(id))
}

/// Parses a snippet destination (`{title}--{id}.md` or `{id}.md`) into its
/// `EntityId`. The title may itself contain `--`, so the id is the trailing
/// segment. Returns `None` for any other local `.md` path.
pub fn parse_snippet_link(url: &str) -> Option<EntityId> {
    let file = url.strip_suffix(".md")?;
    let id = file.rsplit("--").next()?;
    let plausible = id.len() == 20 && id.bytes().all(|b| b.is_ascii_alphanumeric());
    plausible.then(|| EntityId::from_string(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_document_embeds_and_image_embeds() {
        let content = "![doc](Hello--0123456789abcdef0123.md) and ![pic](attachments/abcdef0123456789abcd.png)";
        let targets = parse_local_markdown_targets(content);
        assert_eq!(targets.len(), 2);
        assert!(matches!(targets[0].target, LocalMarkdownTarget::Snippet(_)));
        assert!(targets[0].is_image);
        assert!(matches!(targets[1].target, LocalMarkdownTarget::Image(_)));
        assert!(targets[1].is_image);
    }

    #[test]
    fn plain_links_are_not_images() {
        let content = "see [a](Hello--0123456789abcdef0123.md)";
        let targets = parse_local_markdown_targets(content);
        assert_eq!(targets.len(), 1);
        assert!(!targets[0].is_image);
    }

    #[test]
    fn skips_fenced_code_blocks() {
        let content = "```\n![fake](attachments/abcdef0123456789abcd.png)\n```\n![real](attachments/abcdef0123456789abcd.png)";
        let targets = parse_local_markdown_targets(content);
        assert_eq!(targets.len(), 1);
    }

    #[test]
    fn skips_inline_code_spans() {
        let content = "`![fake](attachments/abcdef0123456789abcd.png)` and ![real](attachments/abcdef0123456789abcd.png)";
        let targets = parse_local_markdown_targets(content);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].start, content.rfind("![real]").unwrap());
    }

    #[test]
    fn ignores_scheme_urls_as_unresolved() {
        let content = "![remote](https://example.com/x.png)";
        let targets = parse_local_markdown_targets(content);
        assert_eq!(targets.len(), 1);
        assert!(matches!(
            targets[0].target,
            LocalMarkdownTarget::UnresolvedLocal(_)
        ));
    }

    #[test]
    fn attachment_link_needs_plausible_id() {
        assert!(parse_attachment_link("attachments/abcdef0123456789abcd.png").is_some());
        assert!(parse_attachment_link("attachments/foo.png").is_none());
        assert!(parse_attachment_link("attachments/abcdef0123456789abcd.svg").is_none());
        assert!(parse_attachment_link("attachments/x/abcdef0123456789abcd.png").is_none());
    }

    #[test]
    fn resolves_attachment_and_snippet_ids() {
        assert!(matches!(
            resolve_target("attachments/abcdef0123456789abcd.jpg"),
            LocalMarkdownTarget::Image(_)
        ));
        assert!(matches!(
            resolve_target("Hello--0123456789abcdef0123.md"),
            LocalMarkdownTarget::Snippet(_)
        ));
    }
}
