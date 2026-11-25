// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! How the desktop's searches fold the words they compare, both sides the same
//! way.
//!
//! Case and accents fall away through GLib's Unicode decomposition, which knows
//! every accented Latin letter: a search without accents finds the accented
//! word. A name made for the network loses its accents the same way.

/// `text` in lower case with its accents taken off.
#[must_use]
pub fn fold(text: &str) -> String {
    // Lower case first: some capitals only gain their combining mark when they
    // are lowered (`İ` becomes `i` plus a dot above).
    without_accents(&text.to_lowercase())
}

/// `text` with its accents taken off and its case kept: "Cássia" is "Cassia".
#[must_use]
pub fn without_accents(text: &str) -> String {
    // `Default` is canonical decomposition (NFD): "ç" becomes "c" and a
    // combining cedilla, which is dropped below.
    glib::normalize(text, glib::NormalizeMode::Default)
        .chars()
        .filter(|character| !('\u{300}'..='\u{36f}').contains(character))
        .collect()
}

/// The words of already-folded `text`: runs of letters and digits.
pub fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_and_accents_fall_away() {
        assert_eq!(fold("Configurações"), "configuracoes");
        assert_eq!(fold("ÁREA de Trabalho"), "area de trabalho");
        assert_eq!(fold("Mañana Größe Øre"), "manana große øre");
        assert_eq!(fold("Relatório"), "relatorio");
        assert_eq!(fold("bigcontrolcenter"), "bigcontrolcenter");
    }

    #[test]
    fn words_are_letter_digit_runs() {
        let found: Vec<&str> = words("br.com.biglinux bigfiles-2").collect();
        assert_eq!(found, ["br", "com", "biglinux", "bigfiles", "2"]);
    }
}
