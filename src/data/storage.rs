use std::{ffi::OsStr, fs, io, path::PathBuf};

use super::Snippet;

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

            let path = entry.path();
            let Some(title) = path.file_stem().and_then(OsStr::to_str) else {
                continue;
            };
            snippets.push(Snippet {
                title: title.to_owned(),
                content: fs::read_to_string(path)?,
            });
        }

        snippets.sort_unstable_by(|left, right| left.title.cmp(&right.title));
        Ok(snippets)
    }

    pub fn load(&self, title: &str) -> io::Result<Snippet> {
        Ok(Snippet {
            title: title.to_owned(),
            content: fs::read_to_string(self.path_for(title)?)?,
        })
    }

    /// Creates the snippet file or replaces its content if it already exists.
    /// 
    /// No atomic guarantee provided.
    pub fn save(&self, snippet: &Snippet) -> io::Result<()> {
        fs::write(self.path_for(&snippet.title)?, &snippet.content)
    }

    pub fn remove(&self, title: &str) -> io::Result<()> {
        fs::remove_file(self.path_for(title)?)
    }

    fn path_for(&self, title: &str) -> io::Result<PathBuf> {
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

        Ok(self.folder.join(title).with_extension(FILE_EXTENSION))
    }
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
            title: "hello".to_owned(),
            content: "hello, world!".to_owned(),
        };

        store.save(&snippet).unwrap();

        assert_eq!(
            fs::read_to_string(folder.0.join("hello.txt")).unwrap(),
            snippet.content
        );
        assert_eq!(store.load("hello").unwrap(), snippet);
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
                    title: "a".to_owned(),
                    content: "first".to_owned(),
                },
                Snippet {
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

        assert_eq!(
            store.load("../outside").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn removes_a_snippet_file() {
        let folder = TestFolder::new();
        let store = SnippetStore::open(&folder.0).unwrap();
        store
            .save(&Snippet {
                title: "temporary".to_owned(),
                content: String::new(),
            })
            .unwrap();

        store.remove("temporary").unwrap();

        assert!(!folder.0.join("temporary.txt").exists());
    }
}
