//! The languages Scrawl can transcribe on Windows, for Settings' and the tray's Language
//! choice (`language` in `config.json`): the 25 Parakeet TDT v3 speaks, all on this PC.

/// The languages Parakeet TDT 0.6b v3 lists (`general.languages` in its GGUF,
/// `speech::PARAKEET_V3`).
pub const PARAKEET_V3: [&str; 25] = [
    "bg", "cs", "da", "de", "el", "en", "es", "et", "fi", "fr", "hr", "hu", "it", "lt", "lv", "mt",
    "nl", "pl", "pt", "ro", "ru", "sk", "sl", "sv", "uk",
];

/// Every language Parakeet speaks, as `(code, name in that language)`, in the order the picker
/// shows them: English and German first, then by name.
pub const OFFERED: [(&str, &str); 25] = [
    ("en", "English"),
    ("de", "Deutsch"),
    ("cs", "Čeština"),
    ("da", "Dansk"),
    ("et", "Eesti"),
    ("es", "Español"),
    ("fr", "Français"),
    ("hr", "Hrvatski"),
    ("it", "Italiano"),
    ("lv", "Latviešu"),
    ("lt", "Lietuvių"),
    ("hu", "Magyar"),
    ("mt", "Malti"),
    ("nl", "Nederlands"),
    ("pl", "Polski"),
    ("pt", "Português"),
    ("ro", "Română"),
    ("sk", "Slovenčina"),
    ("sl", "Slovenščina"),
    ("fi", "Suomi"),
    ("sv", "Svenska"),
    ("el", "Ελληνικά"),
    ("bg", "Български"),
    ("ru", "Русский"),
    ("uk", "Українська"),
];

pub fn parakeet_v3_supports(language: &str) -> bool {
    PARAKEET_V3.contains(&language)
}

/// Whether the picker offers `language`.
pub fn offered(language: &str) -> bool {
    OFFERED.iter().any(|(code, _)| *code == language)
}

/// The language's own name (`Deutsch`), else its code.
pub fn name(language: &str) -> &str {
    OFFERED
        .iter()
        .find(|(code, _)| *code == language)
        .map_or(language, |(_, name)| name)
}

/// What Settings says when Parakeet can't transcribe `language`.
pub fn parakeet_note(language: &str) -> Option<String> {
    (!parakeet_v3_supports(language)).then(|| {
        format!(
            "Parakeet doesn't speak {}, so recordings it transcribes come out as English.",
            name(language)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picker_offers_every_language_parakeet_speaks_once() {
        let mut codes: Vec<&str> = OFFERED.iter().map(|(code, _)| *code).collect();
        assert_eq!(&codes[..2], ["en", "de"]);
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), OFFERED.len());
        assert_eq!(codes, PARAKEET_V3);
        assert_eq!(name("de"), "Deutsch");
        assert_eq!(name("xx"), "xx");
        assert!(offered("fi") && !offered("ja") && !offered("zh"));
    }

    #[test]
    fn parakeet_notes_a_language_it_cannot_speak() {
        assert_eq!(parakeet_note("de"), None);
        assert_eq!(
            parakeet_note("ja").as_deref(),
            Some("Parakeet doesn't speak ja, so recordings it transcribes come out as English.")
        );
    }

    /// `PARAKEET_V3` matches the languages the downloaded model lists. Reads it from
    /// `SCRAWL_MODELS_DIR` (or Scrawl's own models folder).
    #[test]
    #[ignore = "loads the 740 MB Parakeet v3 model"]
    fn the_v3_list_matches_the_model() {
        let path =
            crate::win::speech::own_models_dir().join(crate::win::speech::PARAKEET_V3.filename);
        transcribe_cpp::init_backends_default().unwrap();
        let model = transcribe_cpp::Model::load(&path).unwrap();
        let mut listed = model.capabilities().languages;
        listed.sort_unstable();
        assert_eq!(listed, PARAKEET_V3);
    }
}
