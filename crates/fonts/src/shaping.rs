//! Arabic, Hebrew and Thai text shaping with the matching craft-fonts face and bounded glyph outlines.

use std::sync::OnceLock;

use harfrust::{BufferClusterLevel, Direction, ShapeOptions, ShaperData, UnicodeBuffer, script};
use skrifa::instance::{LocationRef, Size};
use skrifa::raw::TableProvider;
use skrifa::{FontRef, GlyphId, MetadataProvider};

use crate::{GlyphError, GlyphOutline};

/// One cluster of shaped text: the glyphs that show one character (a letter's body and
/// dots, say), or several characters for a ligature. Em units.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapedCluster {
    /// Face that owns the glyph ids.
    pub script: ShapedScript,
    /// Glyph ids, each with its offset from the cluster's pen position (marks sit above or below).
    pub glyphs: Vec<(u32, [f64; 2])>,
    pub advance: f64,
    /// The characters it shows, in logical order.
    pub text: String,
}

/// The face owning a shaped glyph id. IDs from different faces must never share a cache key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShapedScript {
    Arabic,
    Hebrew,
    Thai,
    /// The `n`th symbol face ([`crate::symbol_fonts`]): one glyph per character, not shaped.
    Symbol(u8),
    /// The `n`th craft-fonts Latin face ([`crate::embedding::field_faces`]) standing in for a font
    /// a form names but doesn't embed (#14): one glyph per character, not shaped.
    Twin(u8),
}

/// How many Latin faces can stand in as twins (the Latin manifest has fewer).
const TWIN_FACES: usize = 64;

/// How many symbol faces are looked in, in manifest order.
const SYMBOL_FACES: u8 = 4;

impl ShapedScript {
    pub fn font(self) -> Option<&'static crate::CraftFont> {
        match self {
            Self::Arabic => crate::document_arabic_font(),
            Self::Hebrew => crate::document_hebrew_font(false),
            Self::Thai => crate::document_thai_font(),
            Self::Symbol(n) => crate::symbol_fonts().get(usize::from(n)).copied(),
            Self::Twin(n) => crate::embedding::field_faces().get(usize::from(n)).map(|f| f.font),
        }
    }
}

fn face(script: ShapedScript) -> Option<&'static (FontRef<'static>, ShaperData)> {
    static FACE: OnceLock<Option<(FontRef<'static>, ShaperData)>> = OnceLock::new();
    static HEBREW: OnceLock<Option<(FontRef<'static>, ShaperData)>> = OnceLock::new();
    static THAI: OnceLock<Option<(FontRef<'static>, ShaperData)>> = OnceLock::new();
    static SYMBOL: [OnceLock<Option<(FontRef<'static>, ShaperData)>>; SYMBOL_FACES as usize] = [const { OnceLock::new() }; SYMBOL_FACES as usize];
    let cache = match script {
        ShapedScript::Arabic => &FACE,
        ShapedScript::Hebrew => &HEBREW,
        ShapedScript::Thai => &THAI,
        ShapedScript::Symbol(n) => SYMBOL.get(usize::from(n))?,
        ShapedScript::Twin(n) => {
            static TWIN: [OnceLock<Option<(FontRef<'static>, ShaperData)>>; TWIN_FACES] = [const { OnceLock::new() }; TWIN_FACES];
            TWIN.get(usize::from(n))?
        }
    };
    cache
        .get_or_init(|| {
            let font = FontRef::new(script.font()?.bytes).ok()?;
            let data = ShaperData::new(&font);
            Some((font, data))
        })
        .as_ref()
}

/// Whether the Arabic document face has a glyph for `c` (false without the face).
pub fn arabic_has(c: char) -> bool {
    face(ShapedScript::Arabic).is_some_and(|(font, _)| font.charmap().map(c).is_some())
}

/// Shape one directional run of `text` with the Arabic document face. The clusters come in
/// drawing order, left to right. [`GlyphError::NoFont`] without the face;
/// [`GlyphError::Missing`] when it lacks a character.
pub fn shape_arabic(text: &str, rtl: bool) -> Result<Vec<ShapedCluster>, GlyphError> {
    shape_script(text, rtl, ShapedScript::Arabic)
}

/// Whether the Hebrew document face has a glyph for `c` (false without the face).
pub fn hebrew_has(c: char) -> bool {
    face(ShapedScript::Hebrew).is_some_and(|(font, _)| font.charmap().map(c).is_some())
}

/// Shape one directional run of `text` with the Hebrew document face, clusters in drawing order.
/// Points (niqqud) and cantillation marks stay in their letter's cluster, positioned by the face.
pub fn shape_hebrew(text: &str, rtl: bool) -> Result<Vec<ShapedCluster>, GlyphError> {
    shape_script(text, rtl, ShapedScript::Hebrew)
}

/// The symbol face with a glyph for `c`, as the [`ShapedScript`] that owns it; `None` without one
/// (or without craft-fonts).
pub fn symbol_face_for(c: char) -> Option<ShapedScript> {
    (0..SYMBOL_FACES).map(ShapedScript::Symbol).find(|s| face(*s).is_some_and(|(font, _)| font.charmap().map(c).is_some()))
}

/// `c` as one cluster in the symbol face that has it: its glyph at the pen and its own advance
/// (em). Symbols need no shaping: one character, one glyph.
pub fn symbol_cluster(c: char) -> Option<ShapedCluster> {
    face_cluster(symbol_face_for(c)?, c)
}

/// `c` as one cluster of the face `script` names (a symbol face or a twin), unshaped: its glyph
/// at the pen and its own advance (em). `None` when the face has no glyph for it.
pub fn face_cluster(script: ShapedScript, c: char) -> Option<ShapedCluster> {
    let (font, _) = face(script)?;
    let id = font.charmap().map(c)?;
    let upem = f64::from(font.head().ok()?.units_per_em().max(1));
    let advance = f64::from(font.glyph_metrics(Size::unscaled(), LocationRef::default()).advance_width(id)?) / upem;
    Some(ShapedCluster { script, glyphs: vec![(id.to_u32(), [0.0, 0.0])], advance, text: c.to_string() })
}

/// The open twin standing in for a font named `base_font` (a `/BaseFont` such as `Calibri`,
/// `ABCDEF+Calibri-Bold` or `TimesNewRomanPS-BoldMT`) in the given weight and posture: a
/// craft-fonts Latin face with the same widths (#14). Calibri is drawn with Carlito, Cambria with
/// Caladea, Arial, Times New Roman and Courier New with Liberation Sans, Serif and Mono. `None`
/// for other fonts, and without the craft-fonts Latin faces.
pub fn twin_for(base_font: &str, bold: bool, italic: bool) -> Option<ShapedScript> {
    let name = base_font.split_once('+').filter(|(tag, _)| tag.len() == 6).map_or(base_font, |(_, rest)| rest);
    let family = name.split(['-', ',']).next().unwrap_or(name);
    let key: String = family.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_ascii_lowercase();
    let key = key.strip_suffix("mt").unwrap_or(&key);
    let key = key.strip_suffix("ps").unwrap_or(key);
    let twin = match key {
        "calibri" => "Carlito",
        "cambria" => "Caladea",
        "arial" => "Liberation Sans",
        "timesnewroman" => "Liberation Serif",
        "couriernew" => "Liberation Mono",
        _ => return None,
    };
    let style = match (bold, italic) {
        (false, false) => "Regular",
        (true, false) => "Bold",
        (false, true) => "Italic",
        (true, true) => "Bold Italic",
    };
    let n = crate::embedding::field_faces().iter().position(|f| f.font.family == twin && f.font.style == style)?;
    u8::try_from(n).ok().filter(|n| usize::from(*n) < TWIN_FACES).map(ShapedScript::Twin)
}

/// The face's own name for a twin, as notes name it (`Carlito Bold`).
pub fn twin_label(script: ShapedScript) -> Option<&'static str> {
    match script {
        ShapedScript::Twin(n) => crate::embedding::field_faces().get(usize::from(n)).map(|f| f.label.as_str()),
        _ => None,
    }
}

pub fn thai_has(c: char) -> bool {
    face(ShapedScript::Thai).is_some_and(|(font, _)| font.charmap().map(c).is_some())
}

/// Shape Thai with grapheme clusters so Sara Am decomposition and stacked marks keep their
/// original Unicode text exactly once, including when wrapped or copied from the saved PDF.
pub fn shape_thai(text: &str) -> Result<Vec<ShapedCluster>, GlyphError> {
    shape_script(text, false, ShapedScript::Thai)
}

fn shape_script(text: &str, rtl: bool, kind: ShapedScript) -> Result<Vec<ShapedCluster>, GlyphError> {
    let (font, data) = face(kind).ok_or(GlyphError::NoFont)?;
    let shaper = data.shaper(font).build();
    let scale = 1.0 / f64::from(shaper.units_per_em().max(1));
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.set_direction(if rtl { Direction::RightToLeft } else { Direction::LeftToRight });
    buffer.set_script(match kind {
        ShapedScript::Arabic => script::ARABIC,
        ShapedScript::Hebrew => script::HEBREW,
        ShapedScript::Thai => script::THAI,
        ShapedScript::Symbol(_) | ShapedScript::Twin(_) => script::COMMON,
    });
    // Preserve the historical Arabic per-character mapping; Hebrew and Thai marks stay with their base.
    buffer.set_cluster_level(match kind {
        ShapedScript::Arabic => BufferClusterLevel::Characters,
        ShapedScript::Hebrew | ShapedScript::Thai | ShapedScript::Symbol(_) | ShapedScript::Twin(_) => BufferClusterLevel::MonotoneGraphemes,
    });
    let shaped = shaper.shape(buffer, ShapeOptions::new());
    // Clusters are byte offsets into `text`; a cluster shows the characters up to the next one.
    let mut starts: Vec<usize> = shaped.glyph_infos().iter().map(|g| g.cluster as usize).collect();
    starts.sort_unstable();
    starts.dedup();
    let mut out: Vec<(usize, ShapedCluster)> = Vec::new();
    for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
        if info.glyph_id == 0 {
            return Err(GlyphError::Missing);
        }
        let start = info.cluster as usize;
        // A cluster's glyphs are adjacent in the shaped run.
        let cluster = match out.last_mut() {
            Some((s, c)) if *s == start => c,
            _ => {
                let end = starts.get(starts.partition_point(|s| *s <= start)).copied().unwrap_or(text.len());
                let text = text.get(start..end).unwrap_or_default().to_string();
                out.push((start, ShapedCluster { script: kind, glyphs: Vec::new(), advance: 0.0, text }));
                // Just pushed.
                let Some((_, c)) = out.last_mut() else { continue };
                c
            }
        };
        let at = [cluster.advance + f64::from(pos.x_offset) * scale, f64::from(pos.y_offset) * scale];
        cluster.glyphs.push((info.glyph_id, at));
        cluster.advance += f64::from(pos.x_advance) * scale;
    }
    Ok(out.into_iter().map(|(_, c)| c).collect())
}

/// Glyph `id` of the Arabic document face, bounded like [`crate::japanese_glyph`]. Glyphs without
/// outlines (spaces) come back empty.
pub fn arabic_glyph(id: u32) -> Result<GlyphOutline, GlyphError> {
    shaped_glyph(ShapedScript::Arabic, id)
}

/// A bounded outline from the same face that produced the shaped glyph id.
pub fn shaped_glyph(script: ShapedScript, id: u32) -> Result<GlyphOutline, GlyphError> {
    let (font, _) = face(script).ok_or(GlyphError::NoFont)?;
    glyph(font, id)
}

/// Glyph `id` of the face in `bytes`, bounded like [`shaped_glyph`]: for text shaped with
/// [`crate::source_shaping::shape`] in a face no [`ShapedScript`] names.
pub fn face_glyph(bytes: &[u8], id: u32) -> Result<GlyphOutline, GlyphError> {
    glyph(&FontRef::new(bytes).map_err(|_| GlyphError::NoFont)?, id)
}

fn glyph(font: &FontRef, id: u32) -> Result<GlyphOutline, GlyphError> {
    if font.maxp().map_or(true, |m| id >= u32::from(m.num_glyphs())) {
        return Err(GlyphError::Missing);
    }
    let gid = GlyphId::new(id);
    let loc = LocationRef::default();
    let scale = 1.0 / font.metrics(Size::unscaled(), loc).units_per_em.max(1) as f64;
    let width = font.glyph_metrics(Size::unscaled(), loc).advance_width(gid).unwrap_or(0.0) as f64 * scale;
    // Marks have no advance; the widest ligatures (U+FDFD) span a few em.
    if !width.is_finite() || !(0.0..=8.0).contains(&width) {
        return Err(GlyphError::Missing);
    }
    if font.outline_glyphs().get(gid).is_none() {
        return Ok(GlyphOutline { contours: Vec::new(), width, bbox: [0.0, 0.0, width, 0.0] });
    }
    crate::script::bounded_outline(font, gid, width)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face_or_skip() -> bool {
        if crate::document_arabic_font().is_none() {
            eprintln!("skipping the Arabic shaping checks: built without a craft-fonts Arab face (set CRAFT_FONTS_DIR)");
            assert_eq!(shape_arabic("ب", true), Err(GlyphError::NoFont));
            assert_eq!(arabic_glyph(1).map(|_| ()), Err(GlyphError::NoFont));
            assert!(!arabic_has('ب'));
            return false;
        }
        true
    }

    #[test]
    fn letters_take_their_joining_forms() {
        if !face_or_skip() {
            return;
        }
        let ids =
            |s: &str| shape_arabic(s, true).unwrap().iter().flat_map(|c| c.glyphs.iter().map(|g| g.0)).collect::<std::collections::BTreeSet<_>>();
        let (isolated, joined) = (ids("ب"), ids("ببب"));
        // Initial, medial and final forms are glyphs the isolated letter doesn't use.
        assert!(joined.difference(&isolated).count() >= 2, "{isolated:?} {joined:?}");
        // One cluster per letter, however many glyphs the face draws it with (Noto Sans Arabic:
        // body and dots).
        let c = shape_arabic("ببب", true).unwrap();
        assert!(c.len() == 3 && c.iter().all(|c| c.text == "ب" && c.advance > 0.0 && !c.glyphs.is_empty()), "{c:?}");
    }

    #[test]
    fn right_to_left_runs_are_drawn_from_their_last_letter() {
        if !face_or_skip() {
            return;
        }
        let g = shape_arabic("سلام", true).unwrap();
        let drawn: String = g.iter().map(|g| g.text.as_str()).collect();
        assert_eq!(drawn.chars().rev().collect::<String>(), "سلام");
        assert!(shape_arabic("سلام", false).unwrap().iter().map(|g| g.text.as_str()).collect::<String>() == "سلام");
    }

    #[test]
    fn marks_keep_their_own_text_and_no_advance() {
        if !face_or_skip() {
            return;
        }
        let c = shape_arabic("بَ", true).unwrap();
        assert_eq!(c.len(), 2, "{c:?}");
        let mark = c.iter().find(|c| c.text == "\u{064E}").unwrap();
        assert_eq!(mark.advance, 0.0);
        let outline = arabic_glyph(mark.glyphs[0].0).unwrap();
        assert!(!outline.contours.is_empty() && outline.bbox[1] > 0.0, "the fatha sits above the baseline");
    }

    #[test]
    fn spaces_and_missing_characters() {
        if !face_or_skip() {
            return;
        }
        let space = shape_arabic(" ", true).unwrap();
        assert!(space[0].advance > 0.0);
        assert!(arabic_glyph(space[0].glyphs[0].0).unwrap().contours.is_empty());
        assert!(arabic_has('ب') && !arabic_has('日'));
        assert_eq!(shape_arabic("日", true), Err(GlyphError::Missing));
        assert_eq!(arabic_glyph(u32::MAX).map(|_| ()), Err(GlyphError::Missing));
        assert!(shape_arabic("", true).unwrap().is_empty());
    }
}

#[cfg(test)]
mod thai_tests {
    use super::*;

    #[test]
    fn thai_clusters_preserve_sara_am_and_stacked_marks() {
        if crate::document_thai_font().is_none() {
            assert_eq!(shape_thai("น้ำ"), Err(GlyphError::NoFont));
            assert!(!thai_has('ก'));
            return;
        }
        for text in ["น้ำ", "กิ่", "ผู้ใช้", "ปู่", "เก้า", "สวัสดีภาษาไทย", "๑๒๓", ""]
        {
            let clusters = shape_thai(text).unwrap();
            assert_eq!(clusters.iter().map(|c| c.text.as_str()).collect::<String>(), text);
            for c in clusters {
                assert_eq!(c.script, ShapedScript::Thai);
                assert!(c.advance.is_finite() && c.advance >= 0.0);
                for (id, pos) in c.glyphs {
                    assert!(pos.iter().all(|v| v.is_finite()));
                    shaped_glyph(c.script, id).unwrap();
                }
            }
        }
        let stacked = shape_thai("กิ่").unwrap();
        assert_eq!(stacked.len(), 1, "base, vowel and tone must wrap together");
        assert!(stacked[0].glyphs.len() >= 3);
        assert!(stacked[0].glyphs.iter().any(|(_, p)| p[0] != 0.0 || p[1] != 0.0));
        assert_eq!(shape_thai("日"), Err(GlyphError::Missing));
        assert_eq!(shaped_glyph(ShapedScript::Thai, u32::MAX).map(|_| ()), Err(GlyphError::Missing));
    }
}

#[cfg(test)]
mod hebrew_tests {
    use super::*;

    #[test]
    fn hebrew_runs_are_drawn_from_their_last_letter_with_points_on_their_letter() {
        if crate::document_hebrew_font(false).is_none() {
            eprintln!("skipping the Hebrew shaping checks: built without a craft-fonts Hebr face (set CRAFT_FONTS_DIR)");
            assert_eq!(shape_hebrew("ש", true), Err(GlyphError::NoFont));
            assert!(!hebrew_has('ש'));
            return;
        }
        let g = shape_hebrew("שלום", true).unwrap();
        assert!(g.iter().all(|c| c.script == ShapedScript::Hebrew && c.advance > 0.0 && !c.glyphs.is_empty()), "{g:?}");
        let drawn: String = g.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(drawn.chars().rev().collect::<String>(), "שלום");
        // A point (qamats) is one cluster with its letter, so copy and search get it once.
        let pointed = shape_hebrew("שָׁ", true).unwrap();
        assert_eq!(pointed.len(), 1, "{pointed:?}");
        assert_eq!(pointed[0].text, "שָׁ");
        assert!(pointed[0].glyphs.len() >= 2);
        for c in shape_hebrew("שָׁלוֹם 123", true).unwrap() {
            for (id, pos) in c.glyphs {
                assert!(pos.iter().all(|v| v.is_finite()));
                shaped_glyph(c.script, id).unwrap();
            }
        }
        assert!(hebrew_has('א') && hebrew_has('\u{05B8}') && !hebrew_has('日'));
        assert_eq!(shape_hebrew("日", true), Err(GlyphError::Missing));
        assert!(shape_hebrew("", true).unwrap().is_empty());
    }
}
