use std::{ffi::OsStr, fs, io, path::PathBuf};

use super::{EntityId, Snippet};

const FILE_EXTENSION: &str = "txt";

/// Stores each snippet as an ordinary UTF-8 text file in one folder.
#[derive(Clone, Debug)]
pub struct SnippetStore {
    folder: PathBuf,
}

impl SnippetStore {
    pub fn open(folder: impl Into<PathBuf>) -> io::Result<Self> {
        let folder = folder.into();
        fs::create_dir_all(&folder)?;
        Ok(Self { folder })
    }

    pub fn load_all(&self) -> io::Result<Vec<Snippet>> {
        let mut snippets = Vec::new();

        for entry in fs::read_dir(&self.folder)? {
            let entry = entry?;
            if !entry.file_type()?.is_file()
                || entry.path().extension() != Some(OsStr::new(FILE_EXTENSION))
            {
                continue;
            }

            let mut path = entry.path();
            let Some(stem) = path.file_stem().and_then(OsStr::to_str).map(str::to_owned) else {
                continue;
            };
            let (title, id) = match parse_file_stem(&stem) {
                Some(parts) => parts,
                None => {
                    let id = EntityId::new();
                    let new_path = self.path_for(&stem, &id)?;
                    fs::rename(&path, &new_path)?;
                    path = new_path;
                    (stem, id)
                }
            };
            snippets.push(Snippet {
                id,
                title,
                content: fs::read_to_string(path)?,
            });
        }

        snippets.sort_unstable_by(|left, right| left.title.cmp(&right.title));
        Ok(snippets)
    }

    /// Creates the snippet file or replaces its content if it already exists.
    ///
    /// No atomic guarantee provided.
    pub fn save(&self, snippet: &Snippet) -> io::Result<()> {
        let path = self.path_for(&snippet.title, &snippet.id)?;
        fs::write(path, &snippet.content)
    }

    pub fn remove(&self, snippet: &Snippet) -> io::Result<()> {
        fs::remove_file(self.path_for(&snippet.title, &snippet.id)?)
    }

    /// Renames a snippet file atomically. The snippet's identity (`id`) is
    /// unchanged; only the title-derived filename moves from `old_title` to
    /// `new_title`. Use this instead of [`save`](Self::save) to avoid leaving
    /// the old file behind.
    pub fn rename(&self, id: &EntityId, old_title: &str, new_title: &str) -> io::Result<()> {
        let from = self.path_for(old_title, id)?;
        let to = self.path_for(new_title, id)?;
        if from == to {
            return Ok(());
        }
        fs::rename(from, to)
    }

    fn path_for(&self, title: &str, id: &EntityId) -> io::Result<PathBuf> {
        if title.is_empty()
            || title == "."
            || title == ".."
            || title.contains('/')
            || title.contains('\\')
            || title.contains('\0')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "snippet title cannot be empty or contain path separators",
            ));
        }
        if id.as_str().is_empty()
            || id.as_str().contains('/')
            || id.as_str().contains('\\')
            || id.as_str().contains('\0')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "snippet id cannot be empty or contain path separators",
            ));
        }

        Ok(self
            .folder
            .join(format!("{title}--{}", id.as_str()))
            .with_extension(FILE_EXTENSION))
    }
}

fn parse_file_stem(stem: &str) -> Option<(String, EntityId)> {
    let (title, id) = stem.rsplit_once("--")?;
    if title.is_empty() || !is_supported_id(id) {
        return None;
    }
    Some((title.to_owned(), EntityId::from_string(id)))
}

fn is_supported_id(id: &str) -> bool {
    let compact_id = id.len() == 20
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    compact_id || ulid::Ulid::from_string(id).is_ok()
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    struct TestFolder(PathBuf);

    impl TestFolder {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!(
                "floatdea-snippet-store-{}-{nonce}",
                std::process::id()
            )))
        }
    }

    impl Drop for TestFolder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn saves_and_loads_plain_text_files() {
        let folder = TestFolder::new();
        let store = SnippetStore::open(&folder.0).unwrap();
        let snippet = Snippet {
            id: EntityId::new(),
            title: "hello".to_owned(),
            content: "hello, world!".to_owned(),
        };

        store.save(&snippet).unwrap();

        assert_eq!(
            fs::read_to_string(folder.0.join(format!("hello--{}.txt", snippet.id.as_str())))
                .unwrap(),
            snippet.content
        );
        assert_eq!(store.load_all().unwrap(), vec![snippet]);
    }

    #[test]
    fn loads_only_text_files_in_title_order() {
        let folder = TestFolder::new();
        let store = SnippetStore::open(&folder.0).unwrap();
        fs::write(folder.0.join("z.txt"), "last").unwrap();
        fs::write(folder.0.join("a.txt"), "first").unwrap();
        fs::write(folder.0.join("ignored.json"), "{}").unwrap();

        let snippets = store.load_all().unwrap();

        assert_eq!(
            snippets,
            vec![
                Snippet {
                    id: snippets[0].id.clone(),
                    title: "a".to_owned(),
                    content: "first".to_owned(),
                },
                Snippet {
                    id: snippets[1].id.clone(),
                    title: "z".to_owned(),
                    content: "last".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn rejects_titles_that_can_escape_the_folder() {
        let folder = TestFolder::new();
        let store = SnippetStore::open(&folder.0).unwrap();
        let snippet = Snippet {
            id: EntityId::new(),
            title: "../outside".to_owned(),
            content: String::new(),
        };

        assert_eq!(
            store.save(&snippet).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn removes_a_snippet_file() {
        let folder = TestFolder::new();
        let store = SnippetStore::open(&folder.0).unwrap();
        store
            .save(&Snippet {
                id: EntityId::new(),
                title: "temporary".to_owned(),
                content: String::new(),
            })
            .unwrap();

        let snippet = store.load_all().unwrap().remove(0);
        let path = folder
            .0
            .join(format!("temporary--{}.txt", snippet.id.as_str()));
        store.remove(&snippet).unwrap();

        assert!(!path.exists());
    }

    #[test]
    fn continues_to_load_legacy_ulid_filenames() {
        let folder = TestFolder::new();
        let store = SnippetStore::open(&folder.0).unwrap();
        let legacy_id = ulid::Ulid::new().to_string();
        fs::write(folder.0.join(format!("legacy--{legacy_id}.txt")), "content").unwrap();

        let snippets = store.load_all().unwrap();

        assert_eq!(snippets[0].id.as_str(), legacy_id);
        assert_eq!(snippets[0].title, "legacy");
    }

    #[test]
    fn rename_moves_the_file_without_leaving_the_old_one() {
        let folder = TestFolder::new();
        let store = SnippetStore::open(&folder.0).unwrap();
        let snippet = Snippet {
            id: EntityId::new(),
            title: "old".to_owned(),
            content: "body".to_owned(),
        };
        store.save(&snippet).unwrap();
        let old_path = folder
            .0
            .join(format!("old--{}.txt", snippet.id.as_str()));
        let new_path = folder
            .0
            .join(format!("new--{}.txt", snippet.id.as_str()));

        store
            .rename(&snippet.id, "old", "new")
            .unwrap();

        assert!(!old_path.exists(), "old file should be gone after rename");
        assert!(new_path.exists(), "new file should exist after rename");
        assert_eq!(
            fs::read_to_string(&new_path).unwrap(),
            "body",
            "content must be preserved across rename"
        );

        let reloaded = store.load_all().unwrap();
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].title, "new");
        assert_eq!(reloaded[0].content, "body");
    }
}
