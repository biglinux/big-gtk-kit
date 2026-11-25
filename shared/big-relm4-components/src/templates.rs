// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//! The person's templates folder (`~/Templates`, `~/Modelos`) as the "New from
//! Template" submenu offers it: one entry per file, one submenu per subfolder
//! that holds a template, as GNOME Files shows it.
//!
//! Read when a menu opens, off the main loop, so nothing watches the folder.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gio, glib};

/// Subfolders deeper than this are left out, as GNOME Files does.
const LEVELS: usize = 5;
/// Entries per folder: a templates folder pointed at a full one still makes a
/// menu that fits.
const PER_FOLDER: usize = 30;
/// How long a menu waits for the folder before it opens without it.
const WAIT: Duration = Duration::from_millis(200);

/// One entry of the templates folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Template {
    /// A file to copy; `label` is its name without the extension.
    File {
        /// The menu label.
        label: String,
        /// The file to copy.
        path: PathBuf,
    },
    /// A subfolder with at least one template in it.
    Folder {
        /// The submenu label: the folder's name.
        label: String,
        /// The entries inside it, in menu order.
        templates: Vec<Template>,
    },
}

impl Template {
    fn label(&self) -> &str {
        match self {
            Self::File { label, .. } | Self::Folder { label, .. } => label,
        }
    }
}

/// The templates in the person's folder, read off the main loop. Empty when
/// the desktop names no folder, names the home folder itself (an unset
/// `XDG_TEMPLATES_DIR`), or the folder does not answer in time.
pub async fn read() -> Vec<Template> {
    let read = gio::spawn_blocking(|| {
        glib::user_special_dir(glib::UserDirectory::Templates)
            .filter(|folder| *folder != glib::home_dir())
            .map(|folder| read_folder(&folder, LEVELS))
            .unwrap_or_default()
    });
    match glib::future_with_timeout(WAIT, read).await {
        Ok(Ok(templates)) => templates,
        Ok(Err(_)) => Vec::new(),
        Err(_) => {
            log::warn!("the templates folder did not answer; the menu opens without it");
            Vec::new()
        }
    }
}

/// The files and template-holding subfolders of `folder`, by name. Hidden
/// names, backups (`~`), links and names that are not UTF-8 are left out: a
/// menu target carries the path as text, and a copy of a link would be a link.
fn read_folder(folder: &Path, levels: usize) -> Vec<Template> {
    let entries = big_os_kit::directory_listing::read_directory_listing(folder).unwrap_or_default();
    let mut templates = Vec::new();
    for entry in entries {
        let Some(name) = entry.name.to_str().map(str::to_owned) else {
            continue;
        };
        if name.starts_with('.') || name.ends_with('~') {
            continue;
        }
        if entry.is_dir && levels > 1 {
            let inner = read_folder(&entry.path, levels - 1);
            if !inner.is_empty() {
                templates.push(Template::Folder {
                    label: name,
                    templates: inner,
                });
            }
        } else if entry.is_file {
            templates.push(Template::File {
                label: file_label(&name).to_owned(),
                path: entry.path,
            });
        }
    }
    templates.sort_by_cached_key(|template| glib::FilenameCollationKey::from(template.label()));
    templates.truncate(PER_FOLDER);
    templates
}

/// A file is shown by its name without the extension, which the new document
/// keeps.
fn file_label(name: &str) -> &str {
    &name[..big_os_kit::file_kinds::stem_len(name)]
}

/// `templates` as menu entries firing `action` with the template's path.
#[must_use]
pub fn menu(templates: &[Template], action: &str) -> gio::Menu {
    let menu = gio::Menu::new();
    for template in templates {
        match template {
            Template::File { label, path } => {
                let item = gio::MenuItem::new(Some(label), None);
                item.set_action_and_target_value(
                    Some(action),
                    Some(&path.to_string_lossy().to_variant()),
                );
                menu.append_item(&item);
            }
            Template::Folder { label, templates } => {
                menu.append_submenu(Some(label), &self::menu(templates, action));
            }
        }
    }
    menu
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_drop_extensions() {
        assert_eq!(file_label("Planilha.ods"), "Planilha");
        assert_eq!(file_label("Nota"), "Nota");
        assert_eq!(file_label("dois.pontos.txt"), "dois.pontos");
    }

    /// Subfolders become submenus; an empty one, hidden names, backups and
    /// links are left out.
    #[test]
    fn folder_follows_subfolders() {
        let root = std::env::temp_dir().join(format!("big-templates-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Escritório")).unwrap();
        std::fs::create_dir_all(root.join("Vazia")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/config"), "").unwrap();
        std::fs::write(root.join("Escritório/Planilha.ods"), "").unwrap();
        std::fs::write(root.join("Nota.txt"), "").unwrap();
        std::fs::write(root.join("Nota.txt~"), "").unwrap();
        std::fs::write(root.join(".oculto"), "").unwrap();
        std::os::unix::fs::symlink(root.join("Nota.txt"), root.join("Atalho")).unwrap();
        let read = read_folder(&root, LEVELS);
        let shallow = read_folder(&root, 1);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            read,
            [
                Template::Folder {
                    label: "Escritório".to_owned(),
                    templates: vec![Template::File {
                        label: "Planilha".to_owned(),
                        path: root.join("Escritório/Planilha.ods"),
                    }],
                },
                Template::File {
                    label: "Nota".to_owned(),
                    path: root.join("Nota.txt"),
                },
            ]
        );
        assert_eq!(shallow.len(), 1, "no level left for the subfolder");
    }

    #[test]
    fn missing_folder_has_none() {
        assert!(read_folder(Path::new("/nonexistent/templates"), LEVELS).is_empty());
    }

    #[test]
    fn menu_nests_folders() {
        let templates = [
            Template::Folder {
                label: "Escritório".to_owned(),
                templates: vec![Template::File {
                    label: "Planilha".to_owned(),
                    path: PathBuf::from("/t/Escritório/Planilha.ods"),
                }],
            },
            Template::File {
                label: "Nota".to_owned(),
                path: PathBuf::from("/t/Nota.txt"),
            },
        ];
        let menu = menu(&templates, "win.new-from-template");
        assert_eq!(menu.n_items(), 2);
        let inner = menu
            .item_link(0, gio::MENU_LINK_SUBMENU)
            .expect("a submenu for the folder");
        assert_eq!(inner.n_items(), 1);
        let target = menu
            .item_attribute_value(1, gio::MENU_ATTRIBUTE_TARGET, None)
            .and_then(|target| target.get::<String>());
        assert_eq!(target.as_deref(), Some("/t/Nota.txt"));
        assert_eq!(
            menu.item_attribute_value(1, gio::MENU_ATTRIBUTE_ACTION, None)
                .and_then(|action| action.get::<String>())
                .as_deref(),
            Some("win.new-from-template")
        );
    }
}
