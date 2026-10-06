//! Renderer-facing settings and the invalidation matrix derived from them.

/// Colour scheme applied to territory name labels.
///
/// The serde representation is PascalCase (`"White"`, `"Guild"`, ...) and is
/// load-bearing: it is persisted in `localStorage` under `sequoia_settings_v2`.
/// Do not add `rename_all` here without a migration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NameColor {
    White,  // rgba(220, 218, 210, 0.88) — current default
    Guild,  // per-territory guild color (brightened), same as tag line
    Gold,   // rgba(245, 197, 66, 0.88) — matches app accent
    Copper, // rgba(181, 103, 39, 0.88) — warm copper
    Muted,  // rgba(120, 116, 112, 0.78) — subtle/subdued
}

#[cfg(test)]
mod tests {
    use super::NameColor;

    /// The persisted form is PascalCase; users have settings saved against it.
    #[test]
    fn name_color_serde_representation_is_pascal_case() {
        for (value, expected) in [
            (NameColor::White, "\"White\""),
            (NameColor::Guild, "\"Guild\""),
            (NameColor::Gold, "\"Gold\""),
            (NameColor::Copper, "\"Copper\""),
            (NameColor::Muted, "\"Muted\""),
        ] {
            let encoded = serde_json::to_string(&value).expect("serialize");
            assert_eq!(encoded, expected);
            let decoded: NameColor = serde_json::from_str(expected).expect("deserialize");
            assert_eq!(decoded, value);
        }
    }
}
