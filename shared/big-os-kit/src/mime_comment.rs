// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The words the shared MIME database gives a content type, in the person's
//! language.
//!
//! GIO matches the `xml:lang` of each comment against the locale names
//! verbatim. shared-mime-info 2.5 writes BCP 47 tags (`pt-BR`) where those
//! names say `pt_BR`, so through GIO a type with no plain `pt` comment reads in
//! English. [`translated_comment`] accepts both spellings.

use std::path::PathBuf;

/// The comment `content_type` carries in the first of `languages` it has one
/// in, read from `<dir>/mime/<content_type>.xml` in the first of `data_dirs`
/// holding that file.
///
/// `languages` are locale names, most preferred first (`pt_BR.UTF-8`,
/// `pt_BR`, `pt`), as `g_get_language_names` lists them. `None` when the type
/// has no comment in any of them: the untranslated one is the caller's.
///
/// Blocking: reads one small file.
pub fn translated_comment(
    content_type: &str,
    languages: &[impl AsRef<str>],
    data_dirs: &[PathBuf],
) -> Option<String> {
    // The type names a file under each data directory: one `media/subtype`
    // pair, and nothing that climbs out of `mime/`.
    let (media, subtype) = content_type.split_once('/')?;
    if [media, subtype]
        .iter()
        .any(|part| part.is_empty() || part.starts_with('.') || part.contains('/'))
    {
        return None;
    }
    let text = data_dirs.iter().find_map(|dir| {
        std::fs::read_to_string(dir.join("mime").join(media).join(format!("{subtype}.xml"))).ok()
    })?;
    let comments: Vec<(&str, &str)> = text.lines().filter_map(comment_line).collect();
    languages.iter().find_map(|language| {
        let language = language.as_ref();
        let tag = language.replace('_', "-");
        comments
            .iter()
            .find(|(lang, _)| *lang == language || *lang == tag)
            .map(|(_, comment)| unescape(comment))
    })
}

/// `<comment xml:lang="pt-BR">Documento de texto</comment>`, one to a line as
/// update-mime-database writes them, split into its language and its text.
fn comment_line(line: &str) -> Option<(&str, &str)> {
    let rest = line.trim().strip_prefix("<comment xml:lang=\"")?;
    let (lang, rest) = rest.split_once("\">")?;
    Some((lang, rest.strip_suffix("</comment>")?))
}

/// `&amp;` last, so an escaped entity is not read twice.
fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A comment tagged `pt-BR` answers a `pt_BR` locale; a type name that
    /// climbs out of the database reads nothing.
    #[test]
    fn hyphenated_tag_matches_locale() {
        let root = std::env::temp_dir().join(format!("big-os-kit-mime-{}", std::process::id()));
        std::fs::create_dir_all(root.join("mime/text")).unwrap();
        std::fs::write(
            root.join("mime/text/plain.xml"),
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
             <mime-type xmlns=\"http://www.freedesktop.org/standards/shared-mime-info\" type=\"text/plain\">\n\
             \x20 <comment xml:lang=\"ru\">Текстовый документ</comment>\n\
             \x20 <comment xml:lang=\"pt-BR\">Documento de texto &amp; notas</comment>\n\
             \x20 <comment>Text Document</comment>\n\
             </mime-type>\n",
        )
        .unwrap();
        let dirs = [PathBuf::from("/nonexistent"), root.clone()];
        let languages = ["pt_BR.UTF-8", "pt_BR", "pt", "C"];
        assert_eq!(
            translated_comment("text/plain", &languages, &dirs).as_deref(),
            Some("Documento de texto & notas")
        );
        assert_eq!(translated_comment("text/plain", &["C"], &dirs), None);
        assert_eq!(translated_comment("text/../plain", &languages, &dirs), None);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
