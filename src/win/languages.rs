//! The languages Scrawl can transcribe on Windows, for Settings' and the tray's Language
//! choice (`language` in `config.json`). Claude Code dictates 20 of them and Parakeet TDT v3 25,
//! and the two sets differ: a language only Claude knows falls back to the English model when
//! Parakeet transcribes, and one only Parakeet knows greys out Claude Code.

use crate::transcribe::{CLAUDE_ENGINE_TITLE, ClaudeChoice};

/// Claude Code's dictation languages, from
/// <https://code.claude.com/docs/en/voice-dictation#change-the-dictation-language>. Anything else
/// it dictates as English.
pub const CLAUDE: [&str; 20] = [
    "cs", "da", "de", "el", "en", "es", "fr", "hi", "id", "it", "ja", "ko", "nl", "no", "pl", "pt",
    "ru", "sv", "tr", "uk",
];

/// The languages Parakeet TDT 0.6b v3 lists (`general.languages` in its GGUF,
/// `speech::PARAKEET_V3`).
pub const PARAKEET_V3: [&str; 25] = [
    "bg", "cs", "da", "de", "el", "en", "es", "et", "fi", "fr", "hr", "hu", "it", "lt", "lv", "mt",
    "nl", "pl", "pt", "ro", "ru", "sk", "sl", "sv", "uk",
];

/// Every language at least one engine speaks, as `(code, name in that language)`, in the order
/// the picker shows them: English and German first, then by name.
pub const OFFERED: [(&str, &str); 31] = [
    ("en", "English"),
    ("de", "Deutsch"),
    ("id", "Bahasa Indonesia"),
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
    ("no", "Norsk"),
    ("pl", "Polski"),
    ("pt", "Português"),
    ("ro", "Română"),
    ("sk", "Slovenčina"),
    ("sl", "Slovenščina"),
    ("fi", "Suomi"),
    ("sv", "Svenska"),
    ("tr", "Türkçe"),
    ("el", "Ελληνικά"),
    ("bg", "Български"),
    ("ru", "Русский"),
    ("uk", "Українська"),
    ("hi", "हिन्दी"),
    ("ja", "日本語"),
    ("ko", "한국어"),
];

pub fn claude_supports(language: &str) -> bool {
    CLAUDE.contains(&language)
}

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

/// The Claude Code choice, greyed out with the reason when it can't dictate `language`.
pub fn claude_choice_for(choice: ClaudeChoice, language: &str) -> ClaudeChoice {
    if !choice.enabled || claude_supports(language) {
        return choice;
    }
    ClaudeChoice {
        offered: choice.offered,
        title: format!("{CLAUDE_ENGINE_TITLE}: no {} dictation", name(language)),
        enabled: false,
    }
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
    fn the_picker_offers_every_language_either_engine_speaks_once() {
        let mut codes: Vec<&str> = OFFERED.iter().map(|(code, _)| *code).collect();
        assert_eq!(&codes[..2], ["en", "de"]);
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), OFFERED.len());
        let mut both: Vec<&str> = CLAUDE.iter().chain(&PARAKEET_V3).copied().collect();
        both.sort_unstable();
        both.dedup();
        assert_eq!(codes, both);
        assert_eq!(name("de"), "Deutsch");
        assert_eq!(name("xx"), "xx");
        assert!(offered("fi") && offered("ja") && !offered("zh"));
    }

    #[test]
    fn each_engine_knows_its_own_languages() {
        for both in ["en", "de", "fr", "uk"] {
            assert!(
                claude_supports(both) && parakeet_v3_supports(both),
                "{both}"
            );
        }
        for claude_only in ["ja", "ko", "hi", "id", "no", "tr"] {
            assert!(claude_supports(claude_only) && !parakeet_v3_supports(claude_only));
            assert!(parakeet_note(claude_only).is_some());
        }
        for parakeet_only in ["fi", "hu", "bg", "mt"] {
            assert!(!claude_supports(parakeet_only) && parakeet_v3_supports(parakeet_only));
        }
        assert_eq!(parakeet_note("de"), None);
        assert_eq!(
            parakeet_note("ja").as_deref(),
            Some(
                "Parakeet doesn't speak 日本語, so recordings it transcribes come out as English."
            )
        );
    }

    #[test]
    fn claude_is_greyed_out_for_a_language_it_cannot_dictate() {
        let ready = || ClaudeChoice {
            offered: true,
            title: CLAUDE_ENGINE_TITLE.into(),
            enabled: true,
        };
        assert_eq!(claude_choice_for(ready(), "de"), ready());
        let finnish = claude_choice_for(ready(), "fi");
        assert!(finnish.offered && !finnish.enabled);
        assert_eq!(finnish.title, "Claude Code (dictation): no Suomi dictation");
        // A missing claude.exe keeps its own reason.
        let missing = ClaudeChoice {
            offered: true,
            title: "Claude Code (dictation): claude.exe not found on PATH".into(),
            enabled: false,
        };
        assert_eq!(
            claude_choice_for(missing, "fi").title,
            "Claude Code (dictation): claude.exe not found on PATH"
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
