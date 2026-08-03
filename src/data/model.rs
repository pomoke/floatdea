/// A plain-text note.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snippet {
    pub title: String,
    pub content: String,
}
