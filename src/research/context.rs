//! Local files attached to a run. Their text becomes part of the task brief.

use super::store::LocalDoc;
use super::tools;
use anyhow::{Context, bail};
use std::path::Path;

pub const MAX_FILES: usize = 4;
pub const MAX_CHARS: usize = 30_000;

/// Read a `.md`, `.txt` or `.pdf` file; text beyond `MAX_CHARS` is cut.
pub fn load(path: &Path) -> anyhow::Result<LocalDoc> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let text = match ext.as_str() {
        "md" | "markdown" | "txt" => {
            std::fs::read_to_string(path).with_context(|| format!("Couldn't read {}", path.display()))?
        }
        "pdf" => {
            let bytes = std::fs::read(path).with_context(|| format!("Couldn't read {}", path.display()))?;
            pdf_extract::extract_text_from_mem(&bytes)
                .map_err(|e| anyhow::anyhow!("Couldn't read {}: {e}", path.display()))?
        }
        _ => bail!("Unsupported file type: {name} (use .md, .txt or .pdf)."),
    };
    if text.trim().is_empty() {
        bail!("{name} has no text.");
    }
    let truncated = text.chars().count() > MAX_CHARS;
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    Ok(LocalDoc { name, path: path.display().to_string(), text: tools::truncate_chars(&text, MAX_CHARS), truncated })
}

/// Prompt section listing the attached files; empty when there are none.
pub fn block(docs: &[LocalDoc]) -> String {
    if docs.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "\n\nLocal files the user attached. They are the user's own material; cite them as [local: <name>].\n",
    );
    for d in docs {
        s.push_str(&format!("<local_file name=\"{}\">\n{}\n</local_file>\n", d.name, d.text));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rsc-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn loads_text_and_truncates_long_files() {
        let p = tmp("a.md", &"x".repeat(MAX_CHARS + 5));
        let d = load(&p).unwrap();
        assert_eq!(d.name, "a.md");
        assert!(d.truncated && d.text.ends_with("[truncated]"));
        let p = tmp("b.TXT", "hello");
        let d = load(&p).unwrap();
        assert_eq!((d.text.as_str(), d.truncated), ("hello", false));
    }

    #[test]
    fn rejects_unsupported_and_blank_files() {
        let e = load(&tmp("c.docx", "x")).unwrap_err().to_string();
        assert!(e.starts_with("Unsupported file type: c.docx"), "{e}");
        assert_eq!(load(&tmp("d.md", " \n")).unwrap_err().to_string(), "d.md has no text.");
    }

    #[test]
    fn block_is_empty_without_docs_and_tags_each_file() {
        assert_eq!(block(&[]), "");
        let d = LocalDoc { name: "n.md".into(), path: String::new(), text: "T".into(), truncated: false };
        let b = block(&[d]);
        assert!(b.contains("[local: <name>]") && b.contains("<local_file name=\"n.md\">\nT\n</local_file>"));
    }
}
