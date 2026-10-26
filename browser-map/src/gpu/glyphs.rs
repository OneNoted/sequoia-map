//! Label text measured against the glyph atlas, and the HQ owner line chosen with it.
//! Nothing here touches the GPU, so the host build compiles it for its tests.

use sequoia_map_engine::hq_layout::HqCrown;

#[derive(Clone, Copy)]
pub(super) struct GlyphMeta {
    pub(super) uv_rect: [f32; 4],
    pub(super) advance: f32,
    pub(super) draw_offset_x: f32,
    pub(super) draw_width: f32,
    pub(super) draw_offset_y: f32,
    pub(super) draw_height: f32,
}

/// The glyph atlas's characters, metrics and pair kerning, indexed for per-glyph lookups
/// in label layout. A character the atlas lacks draws as `?` and is not kerned.
pub(super) struct GlyphSet {
    /// Atlas position of each ASCII character; `GlyphSet::ABSENT` where there is none.
    ascii: [u8; 128],
    /// Atlas characters outside ASCII and their positions.
    other: Vec<(char, u8)>,
    /// Metrics by atlas position.
    metas: Vec<GlyphMeta>,
    /// Kerning between the glyphs at positions `a` and `b`, at `a * metas.len() + b`.
    kerning: Vec<f32>,
    fallback: Option<u8>,
}

/// A character's glyph and, unless it was substituted, its kerning position.
#[derive(Clone, Copy)]
pub(super) struct GlyphRef<'a> {
    pub(super) meta: &'a GlyphMeta,
    kern_index: Option<u8>,
}

impl GlyphSet {
    const ABSENT: u8 = u8::MAX;

    /// `chars` are the atlas characters in atlas order (a repeated character keeps its last
    /// position); `kerning` is row-major over those positions.
    pub(super) fn new(chars: &[char], metas: Vec<GlyphMeta>, kerning: Vec<f32>) -> Self {
        debug_assert!(chars.len() == metas.len() && chars.len() < usize::from(Self::ABSENT));
        debug_assert_eq!(kerning.len(), metas.len() * metas.len());
        let mut ascii = [Self::ABSENT; 128];
        let mut other: Vec<(char, u8)> = Vec::new();
        for (position, &ch) in chars.iter().enumerate() {
            let position = position as u8;
            if ch.is_ascii() {
                ascii[ch as usize] = position;
            } else if let Some(entry) = other.iter_mut().find(|(known, _)| *known == ch) {
                entry.1 = position;
            } else {
                other.push((ch, position));
            }
        }
        let mut glyphs = Self {
            ascii,
            other,
            metas,
            kerning,
            fallback: None,
        };
        glyphs.fallback = glyphs.position('?');
        glyphs
    }

    fn position(&self, ch: char) -> Option<u8> {
        if ch.is_ascii() {
            Some(self.ascii[ch as usize]).filter(|&position| position != Self::ABSENT)
        } else {
            self.other
                .iter()
                .find(|(known, _)| *known == ch)
                .map(|&(_, position)| position)
        }
    }

    pub(super) fn get(&self, ch: char) -> Option<GlyphRef<'_>> {
        match self.position(ch) {
            Some(position) => Some(GlyphRef {
                meta: &self.metas[usize::from(position)],
                kern_index: Some(position),
            }),
            None => self.fallback.map(|position| GlyphRef {
                meta: &self.metas[usize::from(position)],
                kern_index: None,
            }),
        }
    }

    /// Kerning between consecutive glyphs; nothing when either was substituted.
    pub(super) fn kern(&self, prev: Option<GlyphRef<'_>>, next: GlyphRef<'_>) -> f32 {
        match (prev.and_then(|prev| prev.kern_index), next.kern_index) {
            (Some(a), Some(b)) => self.kerning[usize::from(a) * self.metas.len() + usize::from(b)],
            _ => 0.0,
        }
    }
}

/// The glyphs and spacing a line of label text is measured with.
pub(super) struct TextFit<'a> {
    pub(super) glyphs: &'a GlyphSet,
    pub(super) line_height: f32,
    pub(super) tracking_units: f32,
}

/// The owner line for an HQ: the guild's name when the style asks for it and it fits at no
/// less than `min_size` world units and the atlas has every character in it, the tag
/// otherwise. Returns the text and its size.
pub(super) fn hq_caption<'a>(
    hq: HqCrown,
    guild: &'a sequoia_shared::GuildRef,
    fit: &TextFit,
    caption_size: f32,
    max_width: f32,
    min_size: f32,
) -> (&'a str, f32) {
    let tag = (guild.prefix.trim(), caption_size);
    let name = guild.name.trim();
    if !hq.shows_guild_name()
        || name.is_empty()
        || name.chars().any(|ch| fit.glyphs.position(ch).is_none())
    {
        return tag;
    }
    let units = line_units_with_tracking(name, fit.glyphs, fit.tracking_units);
    if units <= 0.0 {
        return tag;
    }
    let size = caption_size.min(max_width * fit.line_height / units);
    if size >= min_size { (name, size) } else { tag }
}

pub(super) fn line_units_with_tracking(text: &str, glyphs: &GlyphSet, tracking_units: f32) -> f32 {
    let tracking_units = tracking_units.max(0.0);
    let mut units = 0.0f32;
    let mut prev: Option<GlyphRef<'_>> = None;
    for ch in text.chars() {
        let Some(glyph) = glyphs.get(ch) else {
            continue;
        };
        if prev.is_some() {
            units += tracking_units + glyphs.kern(prev, glyph);
        }
        units += glyph.meta.advance;
        prev = Some(glyph);
    }
    units
}

#[cfg(test)]
mod tests {
    use sequoia_map_engine::hq_layout::HqCrown;
    use sequoia_map_engine::settings::HqCrownStyle;
    use sequoia_shared::GuildRef;

    use super::{GlyphMeta, GlyphSet, TextFit, hq_caption};

    /// An atlas of `chars`, each one unit wide and unkerned.
    fn atlas(chars: &str) -> GlyphSet {
        let chars: Vec<char> = chars.chars().collect();
        let meta = GlyphMeta {
            uv_rect: [0.0; 4],
            advance: 1.0,
            draw_offset_x: 0.0,
            draw_width: 1.0,
            draw_offset_y: 0.0,
            draw_height: 1.0,
        };
        let kerning = vec![0.0; chars.len() * chars.len()];
        GlyphSet::new(&chars, vec![meta; chars.len()], kerning)
    }

    fn guild(name: &str, prefix: &str) -> GuildRef {
        GuildRef {
            uuid: String::new(),
            name: name.into(),
            prefix: prefix.into(),
            color: None,
        }
    }

    fn caption<'a>(glyphs: &GlyphSet, guild: &'a GuildRef) -> (&'a str, f32) {
        let fit = TextFit {
            glyphs,
            line_height: 1.0,
            tracking_units: 0.0,
        };
        let crown = HqCrown::new(HqCrownStyle::NameBelow, 1.0);
        hq_caption(crown, guild, &fit, 4.0, 100.0, 1.0)
    }

    #[test]
    fn hq_caption_shows_a_guild_name_the_atlas_covers() {
        let glyphs = atlas("?SEQaeioqu");
        assert_eq!(caption(&glyphs, &guild("Sequoia", "SEQ")), ("Sequoia", 4.0));
    }

    #[test]
    fn hq_caption_uses_the_tag_when_the_name_would_draw_substitutes() {
        let glyphs = atlas("?SEQaeioqu");
        assert!(glyphs.get('é').is_some(), "the atlas draws `é` as `?`");
        assert_eq!(caption(&glyphs, &guild("Séquoia", "SEQ")), ("SEQ", 4.0));
    }
}
