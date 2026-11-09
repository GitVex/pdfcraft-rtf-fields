//! Field appearances (§12.7.4.3 and §12.5.5), execution plan M6.2.
//!
//! Text and choice fields are drawn from their `/DA` (font, size, colour) and the widget's `/MK`
//! (background and border colours) and `/BS` (border width and style), inside a `/Tx BMC … EMC`
//! marked-content block as viewers expect. Supported: single-line, multiline (wrapped), comb,
//! password, quadding, auto font size (`0 Tf`), combo boxes and list boxes (selection shown), and
//! rich text values (`/RV`, read by [`crate::rich`]) in their own sizes and colours, each run in the
//! form's `/DR` font of the family it names where the form has one, else a standard font.
//!
//! The `/DA` font is used when the form's `/DR` defines it as a simple font that can encode the
//! displayed text (using its own encoding and Differences), or as a composite font with a
//! predefined Unicode CMap such as `UniJIS-UTF16-H`, as Japanese forms use. Otherwise (unsupported
//! characters, other composite fonts, missing resources) Helvetica is used. Text the chosen font
//! can't hold (outside WinAnsi in Helvetica, or beyond U+FFFF in a UCS-2 CMap) is drawn as `?`, as
//! Acrobat keeps a value its field font can't show: the value itself is stored as typed,
//! [`field_appearance_drawn`] returns the first such character, and the caller marks the widget
//! (`pdfcraft_fonts::UNDRAWN_MARK`) and asks other viewers to redraw the field
//! (`/NeedAppearances`). Widths are the field font's own (its `/Widths`, or the standard-14
//! metrics), Helvetica's when Helvetica stands in, and one em per full-width character in
//! composite fonts.
//!
//! `/MK /R` (0, 90, 180, 270, counterclockwise) lays the content out in the upright box and sets
//! the appearance `/Matrix` so that box lands on the widget rectangle. Any other value is 0, and
//! rotation 0 leaves `/Matrix` off.

use pdfcraft_cos::{Dict, Document, Object, Stream};
use pdfcraft_fonts::ShapedScript;
use pdfcraft_fonts::pdf::Metrics;
use pdfcraft_fonts::shaped_glyph as shaped_glyph_of;
use pdfcraft_fonts::shaped_text::{self, Piece, ShapedFonts};
use pdfcraft_fonts::{UnicodeCMap, cjk_width, helvetica_width, literal, win_ansi, wrap_with};

use crate::rich::{self, RichValue, Style};
use crate::{Field, FieldKind, Widget, acroform, flags};

/// Format a number for content streams.
fn n(v: f64) -> String {
    let s = format!("{:.3}", if v.abs() < 5e-4 { 0.0 } else { v });
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

/// Format a number for content streams (up to three decimals).
pub fn fmt(v: f64) -> String {
    n(v)
}

/// The font size an auto-sized (`0 Tf`) text field uses for `text` in a `width` × `height` box
/// (inside the padding), measured by `width_of(text, size)`, never under 4 pt. Single-line text
/// takes the size the height gives, as Acrobat Reader draws it however tall the box (the size is
/// implementation-defined, ISO 32000-2 §12.7.4.3), and shrinks until it fits the width; multiline
/// text takes the largest size up to 12 pt whose wrapped lines fit the height. The page
/// appearance and the on-screen editor share this.
pub fn auto_text_size(text: &str, width: f64, height: f64, multiline: bool, width_of: impl Fn(&str, f64) -> f64) -> f64 {
    let wrap_width = if width.is_finite() { width.max(1.0) } else { 1.0 };
    auto_text_size_by(
        width,
        height,
        multiline,
        |size| width_of(text, size),
        |size| wrap_with(text, size, wrap_width, &width_of).len() as f64 * size * 1.15,
    )
}

/// The automatic font size of a comb field (`Field::is_comb`) whose cells are `height` tall inside
/// the padding: 4 to 12 pt, as only the height bounds a one-character cell. The page appearance
/// and the on-screen editor share this.
pub fn comb_auto_text_size(height: f64) -> f64 {
    if height.is_finite() { (height / 1.15).clamp(4.0, 12.0) } else { 4.0 }
}

/// The tallest box single-line auto-size follows, as a sanity bound: the largest page side
/// ISO 32000-1 Annex C recommends.
pub(crate) const MAX_AUTO_HEIGHT: f64 = 14_400.0;

/// [`auto_text_size`] with the text measured by the caller: `line_width(size)` is its width on
/// one line, `wrapped_height(size)` its height wrapped at `width`. The screen measures in its
/// own font, whose widths and line breaks differ from the page's.
pub fn auto_text_size_by(width: f64, height: f64, multiline: bool, line_width: impl Fn(f64) -> f64, wrapped_height: impl Fn(f64) -> f64) -> f64 {
    let (width, height) = (if width.is_finite() { width.max(1.0) } else { 1.0 }, if height.is_finite() { height } else { 0.0 });
    if multiline {
        let mut size = 12.0;
        // A NaN height from the measurer compares false, so it keeps the size it has.
        while size > 4.0 && wrapped_height(size) > height {
            size -= 0.5;
        }
        return size;
    }
    let size = (height.min(MAX_AUTO_HEIGHT) / 1.15).max(4.0);
    let tw = line_width(size);
    if tw.is_finite() && tw > width { (size * width / tw).max(4.0) } else { size }
}

/// How a text field's widget lays its value out in [`field_appearance_drawn`], for asking
/// whether a value fits it ([`TextBox::fits`]): what a field that doesn't scroll (`DoNotScroll`,
/// ISO 32000-2 Table 228) still takes. Built from the same pieces the appearance uses: the `/DA`
/// font from the form's `/DR` (Helvetica where that font can't show the text), the room inside
/// the border and the 2 pt padding, the `/DA` size (or the automatic size), and wrapping for a
/// multi-line field; the two must change together.
#[derive(Clone, Debug, PartialEq)]
pub struct TextBox {
    /// The `/DA` font from `/DR`, when it is one the appearance can draw with.
    font: Option<FontText>,
    /// Width and height inside the border and padding.
    inner_w: f64,
    inner_h: f64,
    /// The `/DA` size; `None` for an automatic (`0 Tf`) size, which shrinks to fit.
    size: Option<f64>,
    multiline: bool,
    password: bool,
    /// A comb field: its cells, not its width, bound it (`/MaxLen`).
    comb: bool,
}

/// The [`TextBox`] widget `w` of text field `f` lays its value out in.
pub fn text_box(doc: &Document, f: &Field, w: &Widget) -> TextBox {
    let wobj = doc.get(w.obj);
    let wd = wobj.as_dict().cloned().unwrap_or_default();
    let (page_w, page_h) = ((w.rect[2] - w.rect[0]).max(1.0), (w.rect[3] - w.rect[1]).max(1.0));
    let place = placement(mk_rotation(doc, &wd), page_w, page_h);
    let da = parse_da(wd.get(b"DA").and_then(|o| doc.resolve(o).as_string().map(|s| s.to_text())).as_deref().unwrap_or(&f.da));
    let pad = 2.0 + border_width(doc, &wd);
    let multiline = f.has(flags::MULTILINE);
    TextBox {
        font: dr_font(doc, &da.font).map(|(_, enc)| enc),
        inner_w: (place.layout_w - 2.0 * pad).max(1.0),
        inner_h: place.layout_h - 2.0 * pad,
        size: (da.size > 0.0 && da.size.is_finite()).then_some(da.size),
        multiline,
        password: f.has(flags::PASSWORD),
        comb: f.is_comb(),
    }
}

impl TextBox {
    /// Whether `text` fits: on one line within the width, or, multi-line, wrapped within the
    /// height, as the appearance draws it. An automatic size shrinks before text stops fitting,
    /// so such a field is full only when the text no longer fits at its smallest size (4 pt). A
    /// comb field always fits (its `/MaxLen` bounds it).
    pub fn fits(&self, text: &str) -> bool {
        if self.comb || text.is_empty() {
            return true;
        }
        let shown: String = if self.password { "*".repeat(text.chars().count()) } else { text.to_string() };
        // The font the appearance would choose for this text.
        let can_show = |enc: &FontText| if self.multiline { shown.split(['\n', '\r']).all(|l| enc.can_encode(l)) } else { enc.can_encode(&shown) };
        let font = self.font.as_ref().filter(|enc| can_show(enc)).unwrap_or(&FontText::WinAnsi);
        let width_of = |t: &str, size: f64| font.width(t, size);
        // The smallest an automatic size goes ([`auto_text_size_by`]).
        let size = self.size.unwrap_or(4.0);
        if self.multiline {
            // As the appearance draws it: the first baseline is 0.85 em below the padded top and
            // a line is drawn while its descent (0.207 em) stays inside the border, so the
            // padding below the last line is not needed: `inner_h` plus 2 pt.
            // Long words break by character, so a line is wider than the box only when it is a
            // single character wider than the box, which the appearance still cuts at the sides.
            let lines = wrap_with(&shown, size, self.inner_w, width_of);
            let above = lines.len().saturating_sub(1) as f64;
            above * size * 1.15 + 1.057 * size <= self.inner_h + 2.0 + 0.01 && lines.iter().all(|l| width_of(l, size) <= self.inner_w + 0.01)
        } else {
            // A line break in a single-line value is drawn as a space.
            let line: String = shown.chars().map(|c| if matches!(c, '\n' | '\r') { ' ' } else { c }).collect();
            width_of(&line, size) <= self.inner_w + 1e-6
        }
    }

    /// An edit from `before` to `after` as a field that doesn't scroll takes it: all of it when
    /// `after` fits, and otherwise as much of the text it inserted (typed or pasted) as still
    /// fits, keeping what it removed removed. Returns the text and where the caret goes (in
    /// characters, after what was kept of the insertion).
    pub fn fitted_edit(&self, before: &str, after: &str) -> (String, usize) {
        let (b, a): (Vec<char>, Vec<char>) = (before.chars().collect(), after.chars().collect());
        // The edit replaced b[p..b.len() - q] with a[p..a.len() - q].
        let p = b.iter().zip(&a).take_while(|(x, y)| x == y).count();
        let most = b.len().min(a.len()).saturating_sub(p);
        let q = b.iter().rev().zip(a.iter().rev()).take(most).take_while(|(x, y)| x == y).count();
        // q <= a.len() - p, so these are in range; `get` keeps it so if that ever changes.
        let end = a.len().saturating_sub(q);
        let (prefix, inserted, suffix) = (a.get(..p).unwrap_or_default(), a.get(p..end).unwrap_or_default(), a.get(end..).unwrap_or_default());
        let with = |k: usize| prefix.iter().chain(inserted.get(..k).unwrap_or_default()).chain(suffix).collect::<String>();
        if self.fits(after) {
            return (after.to_string(), p + inserted.len());
        }
        // The longest of the insertion's first `from..=to` characters that fits, halving: in one
        // font a longer text is never narrower. `None` when even `from` doesn't fit.
        let longest = |from: usize, to: usize| -> Option<usize> {
            if from > to || !self.fits(&with(from)) {
                return None;
            }
            let (mut lo, mut hi) = (from, to);
            while lo < hi {
                let mid = lo + (hi - lo).div_ceil(2);
                if self.fits(&with(mid)) { lo = mid } else { hi = mid - 1 }
            }
            Some(lo)
        };
        // The font can change once, from the form's own to Helvetica, at the first inserted
        // character the form's font can't show: each side of that point is searched on its own.
        let n = inserted.len();
        let kept = match self.font_switch(prefix, inserted, suffix) {
            Some(t) => longest(0, t.saturating_sub(1)).into_iter().chain(longest(t, n)).max(),
            None => longest(0, n),
        }
        .unwrap_or(0);
        (with(kept), p + kept)
    }

    /// Where an insertion changes the font [`Self::fits`] measures in: how much of `inserted`
    /// (between `prefix` and `suffix`) first includes a character the form's own font can't show,
    /// when everything around it can be shown in that font. `None` when the font never changes.
    fn font_switch(&self, prefix: &[char], inserted: &[char], suffix: &[char]) -> Option<usize> {
        let enc = self.font.as_ref().filter(|_| !self.password)?;
        // As `fits` decides: a multi-line value is checked line by line, its breaks aside.
        let shows = |c: &char| (self.multiline && matches!(c, '\n' | '\r')) || enc.can_encode(&c.to_string());
        if !prefix.iter().chain(suffix).all(shows) {
            return None;
        }
        inserted.iter().position(|c| !shows(c)).map(|i| i + 1)
    }
}

/// `/MK /R` as a quadrant, or 0 when it is missing or not a multiple of 90.
pub(crate) fn mk_rotation(doc: &Document, wd: &Dict) -> i64 {
    let Some(raw) = wd.get(b"MK") else { return 0 };
    let mk_obj = doc.resolve(raw);
    let Some(mk) = mk_obj.as_dict() else { return 0 };
    let Some(r_obj) = mk.get(b"R") else { return 0 };
    let resolved = doc.resolve(r_obj);
    let Some(r) = degrees(&resolved) else { return 0 };
    match r.rem_euclid(360) {
        q @ (0 | 90 | 180 | 270) => q,
        _ => 0,
    }
}

fn degrees(o: &Object) -> Option<i64> {
    if let Some(i) = o.as_int() {
        return Some(i);
    }
    let v = o.as_f64()?;
    (v.is_finite() && v.abs() <= 1.0e6).then_some(v.round() as i64)
}

/// Where an appearance is drawn: the upright layout box, and the `/Matrix` that turns it onto
/// the widget rectangle. Rotation 0 has no matrix.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Placement {
    pub layout_w: f64,
    pub layout_h: f64,
    pub matrix: Option<[f64; 6]>,
}

pub(crate) fn placement(rotation: i64, page_w: f64, page_h: f64) -> Placement {
    let (layout_w, layout_h, matrix) = match rotation {
        // (x, y) -> (page_w - y, x): text that ran to the right runs upward.
        90 => (page_h, page_w, Some([0.0, 1.0, -1.0, 0.0, page_w, 0.0])),
        180 => (page_w, page_h, Some([-1.0, 0.0, 0.0, -1.0, page_w, page_h])),
        // (x, y) -> (y, page_h - x): text that ran to the right runs downward.
        270 => (page_h, page_w, Some([0.0, -1.0, 1.0, 0.0, 0.0, page_h])),
        _ => (page_w, page_h, None),
    };
    Placement { layout_w, layout_h, matrix }
}

pub(crate) fn set_form_box(d: &mut Dict, place: Placement) {
    let box_ = [0.0, 0.0, place.layout_w, place.layout_h];
    d.set(b"BBox".to_vec(), Object::Array(box_.iter().map(|v| Object::Real(*v)).collect()));
    if let Some(m) = place.matrix {
        d.set(b"Matrix".to_vec(), Object::Array(m.iter().map(|v| Object::Real(*v)).collect()));
    }
}

/// A parsed default appearance string: font resource name, size (0 = auto) and colour operator.
#[derive(Clone, Debug, PartialEq)]
pub struct Da {
    pub font: String,
    pub size: f64,
    /// The colour operation as written (`0 g`, `1 0 0 rg`, `0 0 0 1 k`).
    pub color: String,
}

pub fn parse_da(da: &str) -> Da {
    let toks: Vec<&str> = da.split_whitespace().collect();
    let mut out = Da { font: "Helv".into(), size: 0.0, color: "0 g".into() };
    for (i, t) in toks.iter().enumerate() {
        match *t {
            "Tf" if i >= 2 => {
                out.font = toks[i - 2].trim_start_matches('/').to_string();
                out.size = toks[i - 1].parse::<f64>().ok().filter(|s| s.is_finite() && *s >= 0.0).unwrap_or(0.0).min(300.0);
            }
            "g" if i >= 1 && toks[i - 1].parse::<f64>().is_ok() => out.color = format!("{} g", toks[i - 1]),
            "rg" if i >= 3 && toks[i - 3..i].iter().all(|x| x.parse::<f64>().is_ok()) => out.color = format!("{} rg", toks[i - 3..i].join(" ")),
            "k" if i >= 4 && toks[i - 4..i].iter().all(|x| x.parse::<f64>().is_ok()) => out.color = format!("{} k", toks[i - 4..i].join(" ")),
            _ => {}
        }
    }
    out
}

/// How a field font's text is encoded and measured.
#[derive(Clone, Debug, PartialEq)]
enum FontText {
    WinAnsi,
    /// A reused simple font's encoding, including indirect dictionaries and Differences.
    Simple(Box<Metrics>),
    /// A composite font with a predefined Unicode CMap.
    Unicode(UnicodeCMap),
}

impl FontText {
    fn encode(&self, s: &str) -> Vec<u8> {
        match self {
            Self::WinAnsi => win_ansi(s),
            // Selection checks every displayed string first; wrapping only removes line breaks.
            Self::Simple(metrics) => metrics.encode(&s.replace('\t', " ")).unwrap_or_default(),
            Self::Unicode(cmap) => cmap.encode(s),
        }
    }

    fn can_encode(&self, s: &str) -> bool {
        match self {
            Self::Simple(metrics) => metrics.encode(&s.replace('\t', " ")).is_some(),
            _ => true,
        }
    }

    /// The first character of `s` the encoding can't hold, which [`Self::encode`] writes as `?`
    /// (a reused simple font is only chosen when it holds them all): reported by
    /// [`field_appearance_drawn`], so that `?` is never silent (AGENTS.md §4).
    fn first_unencodable(&self, s: &str) -> Option<char> {
        match self {
            Self::WinAnsi => pdfcraft_fonts::first_undrawable(s),
            Self::Simple(metrics) => {
                s.chars().map(|c| if c == '\t' { ' ' } else { c }).find(|c| metrics.encode(c.encode_utf8(&mut [0; 4])).is_none())
            }
            Self::Unicode(cmap) => cmap.first_unencodable(s),
        }
    }

    /// Whether every character of `s` has a glyph to draw: encodable, and (in a simple font) not
    /// left out of the font, as a subset leaves glyphs it doesn't use at width 0.
    fn draws(&self, s: &str) -> bool {
        match self {
            Self::Simple(metrics) => s.chars().all(|c| {
                let c = if c == '\t' { ' ' } else { c };
                metrics
                    .encode(c.encode_utf8(&mut [0; 4]))
                    .is_some_and(|bytes| c.is_whitespace() || metrics.codes(&bytes).iter().all(|&(code, _)| metrics.width(code) > 0.0))
            }),
            _ => self.first_unencodable(s).is_none(),
        }
    }

    fn width(&self, s: &str, size: f64) -> f64 {
        match self {
            Self::WinAnsi => helvetica_width(s, size),
            // The font's `/Widths`, or its standard-14 metrics (Courier, Times) when it has none.
            Self::Simple(metrics) => match metrics.encode(&s.replace(['\t', '\r', '\n'], " ")) {
                Some(bytes) => metrics.codes(&bytes).iter().map(|&(code, _)| metrics.width(code)).sum::<f64>() * size,
                None => helvetica_width(s, size),
            },
            Self::Unicode(_) => cjk_width(s, size),
        }
    }
}

/// The font resource for `name` from the form's `/DR`, if it is a font we can encode for.
fn dr_font(doc: &Document, name: &str) -> Option<(Object, FontText)> {
    let af = acroform(doc)?;
    let dr = doc.resolve(af.get(b"DR")?);
    let fonts = doc.resolve(dr.as_dict()?.get(b"Font")?);
    let entry = fonts.as_dict()?.get(name.as_bytes())?.clone();
    let font = doc.resolve(&entry);
    let fd = font.as_dict()?;
    match fd.name(b"Subtype") {
        Some(b"Type1" | b"TrueType" | b"MMType1") => {}
        // Japanese, Chinese and Korean forms: a CID font addressed by Unicode (a non-embedded
        // `/HeiseiMin-W3` with `/UniJIS-UCS2-H`, say). Embedded CMap streams and Identity-H need
        // the font's own mapping, so those still fall back to Helvetica.
        Some(b"Type0") => return Some((entry, FontText::Unicode(UnicodeCMap::from_name(fd.name(b"Encoding")?)?))),
        _ => return None,
    }
    // Symbolic fonts (ZapfDingbats, Symbol) can't show WinAnsi text.
    if matches!(fd.name(b"BaseFont"), Some(b"ZapfDingbats" | b"Symbol")) {
        return None;
    }
    Some((entry, FontText::Simple(Box::new(Metrics::from_dict(doc, fd)))))
}

/// A push button's caption, centered in a `width` × `height` box, in the font, size and color its
/// `/DA` asks for: the font from the form's `/DR`, else Helvetica, and the size `auto` when the
/// `/DA` leaves it to the viewer (0). Returns the content operators, the font resource (name and
/// object), and the first character drawn as `?` because the font can't show it.
pub(crate) fn button_caption(doc: &Document, da: &str, caption: &str, width: f64, height: f64, auto: f64) -> (Vec<u8>, String, Object, Option<char>) {
    let da = parse_da(da);
    let (name, font, enc) = match dr_font(doc, &da.font).filter(|(_, enc)| enc.can_encode(caption)) {
        Some((o, enc)) => (da.font.clone(), o, enc),
        None => ("Helv".to_string(), helvetica(), FontText::WinAnsi),
    };
    let missing = enc.first_unencodable(caption);
    let size = if da.size > 0.0 { da.size } else { auto };
    let tw = enc.width(caption, size);
    let mut out =
        format!("BT /{name} {size:.2} Tf {} 1 0 0 1 {:.3} {:.3} Tm ", da.color, (width - tw) / 2.0, (height - size * 0.7) / 2.0).into_bytes();
    out.extend(literal(&enc.encode(caption)));
    out.extend_from_slice(b" Tj ET\n");
    (out, name, font, missing)
}

/// A non-embedded Helvetica font dictionary in WinAnsi encoding, the fallback when the form's
/// `/DR` has no font that can show the text.
fn helvetica() -> Object {
    let mut f = Dict::new();
    f.set(b"Type".to_vec(), Object::name("Font"));
    f.set(b"Subtype".to_vec(), Object::name("Type1"));
    f.set(b"BaseFont".to_vec(), Object::name("Helvetica"));
    f.set(b"Encoding".to_vec(), Object::name("WinAnsiEncoding"));
    Object::Dict(f)
}

/// An `/MK /BC` or `/BG` colour (ISO 32000-2 Table 192) as a fill operator. A four-entry array
/// with a non-number in it, such as `[1 0 0 null]`, is CMYK with that entry 0, as annotation `/C`
/// colours are read.
fn color_array(doc: &Document, mk: Option<&Dict>, key: &[u8]) -> Option<String> {
    let a = doc.resolve(mk?.get(key)?);
    let a = a.as_array()?;
    let v: Vec<f64> = if a.len() == 4 {
        a.iter().map(|x| doc.resolve(x).as_f64().unwrap_or(0.0)).collect()
    } else {
        a.iter().filter_map(|x| doc.resolve(x).as_f64()).collect()
    };
    match v.as_slice() {
        [g] => Some(format!("{} g", n(*g))),
        [r, g, b] => Some(format!("{} {} {} rg", n(*r), n(*g), n(*b))),
        [c, m, y, k] => Some(format!("{} {} {} {} k", n(*c), n(*m), n(*y), n(*k))),
        _ => None,
    }
}

fn stroke_op(fill: &str) -> String {
    match fill.rsplit_once(' ') {
        Some((nums, "g")) => format!("{nums} G"),
        Some((nums, "rg")) => format!("{nums} RG"),
        Some((nums, "k")) => format!("{nums} K"),
        _ => "0 G".into(),
    }
}

/// Background and border from `/MK` and `/BS`; returns (content, border width). A round XFA
/// check button's ([`is_round`]) are a circle inside the widget instead of its rectangle.
fn frame(doc: &Document, wd: &Dict, w: f64, h: f64) -> (String, f64) {
    let mk = wd.get(b"MK").map(|m| doc.resolve(m)).and_then(|m| m.as_dict().cloned());
    let mut c = String::new();
    if is_round(doc, wd) {
        return round_frame(doc, wd, mk.as_ref(), w, h);
    }
    if let Some(bg) = color_array(doc, mk.as_ref(), b"BG") {
        c.push_str(&format!("{bg}\n0 0 {} {} re f\n", n(w), n(h)));
    }
    let bs = wd.get(b"BS").map(|b| doc.resolve(b)).and_then(|b| b.as_dict().cloned());
    let bw = bs.as_ref().and_then(|b| b.get(b"W")).and_then(|x| doc.resolve(x).as_f64()).unwrap_or(1.0).max(0.0);
    if let Some(bc) = color_array(doc, mk.as_ref(), b"BC")
        && bw > 0.0
    {
        let style = bs.as_ref().and_then(|b| b.name(b"S").map(<[u8]>::to_vec)).unwrap_or_default();
        if style == b"U" {
            // Underline: only the bottom edge.
            c.push_str(&format!("{}\n{} w\n0 {} m {} {} l S\n", stroke_op(&bc), n(bw), n(bw / 2.0), n(w), n(bw / 2.0)));
            return (c, bw);
        }
        let dashed = style == b"D";
        c.push_str(&format!(
            "{}\n{} w\n{}{} {} {} {} re S\n[] 0 d\n",
            stroke_op(&bc),
            n(bw),
            if dashed { "[3] 0 d\n" } else { "" },
            n(bw / 2.0),
            n(bw / 2.0),
            n(w - bw),
            n(h - bw)
        ));
        if style == b"B" || style == b"I" {
            // Beveled: a light top-left and a darker bottom-right inside the border; inset:
            // grey top-left and light grey bottom-right.
            let (tl, br) = if style == b"B" { ("1 g", "0.5 g") } else { ("0.5 g", "0.75 g") };
            let (x0, y0, x1, y1) = (bw, bw, w - bw, h - bw);
            let k = bw;
            c.push_str(&format!(
                "{tl}\n{} {} m {} {} l {} {} l {} {} l {} {} l {} {} l f\n{br}\n{} {} m {} {} l {} {} l {} {} l {} {} l {} {} l f\n",
                n(x0),
                n(y0),
                n(x0),
                n(y1),
                n(x1),
                n(y1),
                n(x1 - k),
                n(y1 - k),
                n(x0 + k),
                n(y1 - k),
                n(x0 + k),
                n(y0 + k),
                n(x1),
                n(y1),
                n(x1),
                n(y0),
                n(x0),
                n(y0),
                n(x0 + k),
                n(y0 + k),
                n(x1 - k),
                n(y0 + k),
                n(x1 - k),
                n(y1 - k)
            ));
            return (c, 2.0 * bw);
        }
        return (c, bw);
    }
    (c, 0.0)
}

/// Whether `wd` is a check box or radio button PdfCraft laid out for a round XFA check button
/// (`/PCXfaRound true`, written by the XFA layout).
pub(crate) fn is_round(doc: &Document, wd: &Dict) -> bool {
    matches!(wd.get(b"PCXfaRound").map(|o| doc.resolve(o)).as_deref(), Some(Object::Bool(true)))
}

/// [`frame`] for a round check button: the `/MK /BG` background as a disc and the `/MK /BC`
/// border as a circle, both centred in the `w` × `h` box and as wide as its shorter side. A
/// dashed border stays dashed and an underline stays an underline; any other style is drawn
/// solid, and a border as wide as the circle fills it. Returns the border width.
fn round_frame(doc: &Document, wd: &Dict, mk: Option<&Dict>, w: f64, h: f64) -> (String, f64) {
    let mut c = String::new();
    let (cx, cy, r) = (w / 2.0, h / 2.0, w.min(h).max(0.0) / 2.0);
    if let Some(bg) = color_array(doc, mk, b"BG") {
        c.push_str(&format!("{bg}\n{}f\n", circle_path(cx, cy, r)));
    }
    let bs = wd.get(b"BS").map(|b| doc.resolve(b)).and_then(|b| b.as_dict().cloned());
    let bw = bs.as_ref().and_then(|b| b.get(b"W")).and_then(|x| doc.resolve(x).as_f64()).unwrap_or(1.0).max(0.0);
    let style = bs.as_ref().and_then(|b| b.name(b"S").map(<[u8]>::to_vec)).unwrap_or_default();
    match color_array(doc, mk, b"BC") {
        // An underline stays one: only the bottom edge, as for any widget.
        Some(bc) if bw > 0.0 && style == b"U" => {
            c.push_str(&format!("{}\n{} w\n0 {} m {} {} l S\n", stroke_op(&bc), n(bw), n(bw / 2.0), n(w), n(bw / 2.0)));
            (c, bw)
        }
        // A border as wide as the circle fills it: a disc in the border's colour.
        Some(bc) if bw > 0.0 && bw >= 2.0 * r => {
            c.push_str(&format!("{bc}\n{}f\n", circle_path(cx, cy, r)));
            (c, 2.0 * r)
        }
        Some(bc) if bw > 0.0 => {
            let dashed = style == b"D";
            // The stroke is centred on the path: inset by half its width, it stays inside.
            let path = circle_path(cx, cy, r - bw / 2.0);
            c.push_str(&format!("{}\n{} w\n{}{path}S\n[] 0 d\n", stroke_op(&bc), n(bw), if dashed { "[3] 0 d\n" } else { "" }));
            (c, bw)
        }
        _ => (c, 0.0),
    }
}

/// A closed circle of radius `r` around (`cx`, `cy`) as four Bézier arcs, ending with a space
/// so a paint operator can follow.
fn circle_path(cx: f64, cy: f64, r: f64) -> String {
    // The control-point distance for a quarter circle.
    let k = 0.5523 * r;
    format!(
        "{} {} m {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c ",
        n(cx + r),
        n(cy),
        n(cx + r),
        n(cy + k),
        n(cx + k),
        n(cy + r),
        n(cx),
        n(cy + r),
        n(cx - k),
        n(cy + r),
        n(cx - r),
        n(cy + k),
        n(cx - r),
        n(cy),
        n(cx - r),
        n(cy - k),
        n(cx - k),
        n(cy - r),
        n(cx),
        n(cy - r),
        n(cx + k),
        n(cy - r),
        n(cx + r),
        n(cy - k),
        n(cx + r),
        n(cy)
    )
}

/// The width [`frame`] insets the content by: the `/BS` border width when the widget has a
/// border colour, doubled for beveled and inset borders, and 0 without a border.
fn border_width(doc: &Document, wd: &Dict) -> f64 {
    let mk = wd.get(b"MK").map(|m| doc.resolve(m)).and_then(|m| m.as_dict().cloned());
    let bs = wd.get(b"BS").map(|b| doc.resolve(b)).and_then(|b| b.as_dict().cloned());
    let bw = line_width(doc, wd);
    if color_array(doc, mk.as_ref(), b"BC").is_none() || bw <= 0.0 {
        return 0.0;
    }
    let style = bs.as_ref().and_then(|b| b.name(b"S").map(<[u8]>::to_vec)).unwrap_or_default();
    if style == b"B" || style == b"I" { 2.0 * bw } else { bw }
}

/// The widget's border line width, `/BS /W` (default 1, Table 168), as [`frame`] strokes it.
fn line_width(doc: &Document, wd: &Dict) -> f64 {
    let bs = wd.get(b"BS").map(|b| doc.resolve(b)).and_then(|b| b.as_dict().cloned());
    // `f64::max` turns NaN into 0, as it does a negative width.
    bs.as_ref().and_then(|b| b.get(b"W")).and_then(|x| doc.resolve(x).as_f64()).unwrap_or(1.0).max(0.0)
}

/// Comb fields stop drawing their cells past this many; they would be a solid block anyway.
const MAX_COMB_DIVIDERS: usize = 500;

/// The lines between the `cells` positions of a comb field `w` × `h` (ISO 32000-2 Table 228,
/// Comb: the field is divided into `MaxLen` equally spaced positions), in the border's colour and
/// width, inside the border. None without a border, or past [`MAX_COMB_DIVIDERS`] cells.
fn comb_dividers(doc: &Document, wd: &Dict, w: f64, h: f64, cells: usize) -> String {
    let mk = wd.get(b"MK").map(|m| doc.resolve(m)).and_then(|m| m.as_dict().cloned());
    let inset = border_width(doc, wd);
    let lw = line_width(doc, wd);
    let Some(bc) = color_array(doc, mk.as_ref(), b"BC") else { return String::new() };
    if !(inset > 0.0 && inset.is_finite() && lw > 0.0 && lw.is_finite() && (2..=MAX_COMB_DIVIDERS).contains(&cells)) {
        return String::new();
    }
    let cell = w / cells as f64;
    let mut c = format!("{}\n{} w\n", stroke_op(&bc), n(lw));
    for i in 1..cells {
        let x = cell * i as f64;
        c.push_str(&format!("{} {} m {} {} l\n", n(x), n(inset), n(x), n(h - inset)));
    }
    c.push_str("S\n");
    c
}

/// Where a list box draws its options: one row per option from the top, starting with option
/// `top` (`/TI`, §12.7.4.4). The appearance PdfCraft writes and the viewer's in-place list share
/// it, so a click picks the option drawn under the pointer (#1524).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ListRows {
    /// The first option shown (`/TI`).
    pub top: usize,
    /// From the widget's top edge to the first row, in points.
    pub inset: f64,
    /// The height of one row, in points.
    pub row: f64,
    /// The text size, in points.
    pub size: f64,
}

impl ListRows {
    /// How many whole rows fit in a list box `height` points tall (at least one).
    pub fn visible(&self, height: f64) -> usize {
        if !(height.is_finite() && self.row > 0.0) {
            return 1;
        }
        let rows = ((height - self.inset) / self.row).floor();
        if rows >= 1.0 { rows.min(10_000.0) as usize } else { 1 }
    }

    /// The option at `y` points below the widget's top edge when the list starts at option `top`,
    /// or `None` above the first row. The caller checks it against the number of options.
    pub fn option_at(&self, top: usize, y: f64) -> Option<usize> {
        if !(y.is_finite() && self.row > 0.0) || y < self.inset {
            return None;
        }
        let k = ((y - self.inset) / self.row).floor();
        (k < 1.0e6).then(|| top.saturating_add(k as usize))
    }

    /// The top index that shows as many of the `selected` option indices as fit in `visible`
    /// rows, staying at `current` when they are all shown already, among `count` options.
    pub fn top_showing(current: usize, selected: &[usize], visible: usize, count: usize) -> usize {
        let visible = visible.max(1);
        let last_top = count.saturating_sub(visible);
        let (Some(&first), Some(&last)) = (selected.iter().min(), selected.iter().max()) else { return current.min(last_top) };
        let top = if first >= current && last < current.saturating_add(visible) {
            current
        } else if last.saturating_sub(first) >= visible || first < current {
            first
        } else {
            // Scroll down just far enough that the last selected option is the bottom row.
            last.saturating_add(1).saturating_sub(visible)
        };
        top.min(last_top)
    }
}

/// `/TI` of a list box: on the widget, else on the field.
fn top_index(doc: &Document, wd: &Dict, field: Option<&Dict>) -> usize {
    let ti = |d: &Dict| d.get(b"TI").and_then(|o| doc.resolve(o).as_int());
    ti(wd).or_else(|| field.and_then(ti)).and_then(|i| usize::try_from(i).ok()).unwrap_or(0)
}

/// The rows of a list box whose field's `/DA` is `f_da`, in widget `wd`.
pub(crate) fn list_rows_of(doc: &Document, f_da: &str, field: Option<&Dict>, wd: &Dict) -> ListRows {
    let da = parse_da(wd.get(b"DA").and_then(|o| doc.resolve(o).as_string().map(|s| s.to_text())).as_deref().unwrap_or(f_da));
    let size = if da.size > 0.0 { da.size } else { 12.0 };
    ListRows { top: top_index(doc, wd, field), inset: 2.0 + border_width(doc, wd), row: size * 1.15, size }
}

/// The rows of list box `f` as drawn in widget `w`.
pub fn list_rows(doc: &Document, f: &Field, w: &Widget) -> ListRows {
    let wobj = doc.get(w.obj);
    let wd = wobj.as_dict().cloned().unwrap_or_default();
    let fobj = doc.get(f.obj);
    list_rows_of(doc, &f.da, fobj.as_dict(), &wd)
}

/// The appearance of a text or choice field's widget showing `values`. Characters its font can't
/// hold come out as `?`; [`field_appearance_drawn`] says which.
pub fn field_appearance(doc: &Document, f: &Field, w: &Widget, values: &[String]) -> Stream {
    field_appearance_as(doc, f, w, values, true)
}

/// [`field_appearance`], where `format` false means `values` are already what to show (a
/// custom Format script ran).
pub fn field_appearance_as(doc: &Document, f: &Field, w: &Widget, values: &[String], format: bool) -> Stream {
    field_appearance_drawn(doc, f, w, values, format).0
}

/// [`field_appearance_as`] with the first character it drew as `?` because the field's font can't
/// hold it: the form's font is a simple (WinAnsi) font, Helvetica stands in for one PdfCraft can't
/// write with (an embedded subset, `Identity-H`), or a UCS-2 CMap meets a character beyond U+FFFF.
pub fn field_appearance_drawn(doc: &Document, f: &Field, w: &Widget, values: &[String], format: bool) -> (Stream, Option<char>) {
    let (stream, unencodable, _) = field_appearance_fit(doc, f, w, values, format);
    (stream, unencodable)
}

/// A text value that its box cuts off at the field's font size (#4018).
#[derive(Clone, Debug, PartialEq)]
pub enum Overflow {
    /// One line `width` points wide, with `room` for it, at `size` points.
    Wide { width: f64, room: f64, size: f64 },
    /// `lines` wrapped lines, of which the box shows `room`, at `size` points.
    Tall { lines: usize, room: usize, size: f64 },
    /// One line `height` points high (ascent to descent), with `room` for it, at `size` points.
    High { height: f64, room: f64, size: f64 },
}

impl std::fmt::Display for Overflow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wide { width, room, size } => {
                write!(f, "the value does not fit: {} pt wide at {} pt, room for {} pt", fmt(*width), fmt(*size), fmt(*room))
            }
            Self::Tall { lines, room, size } => write!(f, "the value does not fit: {lines} lines at {} pt, room for {room}", fmt(*size)),
            Self::High { height, room, size } => {
                write!(f, "the value does not fit: {} pt high at {} pt, room for {} pt", fmt(*height), fmt(*size), fmt(*room))
            }
        }
    }
}

/// Whether the text field's value, drawn in widget `w`, is cut off by its box.
pub fn value_overflow(doc: &Document, f: &Field, w: &Widget) -> Option<Overflow> {
    if f.kind != FieldKind::Text {
        return None;
    }
    field_appearance_fit(doc, f, w, &f.value, true).2
}

/// Whether `text` draws in widget `wd` of `f` with no `?`: in the field's `/DR` font when that
/// holds it, as [`field_appearance_fit`] picks, or else in the Helvetica (WinAnsi) stand-in.
fn draws(doc: &Document, f: &Field, wd: &Dict, text: &str) -> bool {
    let da = parse_da(wd.get(b"DA").and_then(|o| doc.resolve(o).as_string().map(|s| s.to_text())).as_deref().unwrap_or(&f.da));
    let enc = dr_font(doc, &da.font).map(|(_, enc)| enc).filter(|enc| enc.can_encode(text)).unwrap_or(FontText::WinAnsi);
    enc.first_unencodable(text).is_none()
}

/// Why text field `f` can't take `text`: it has Thai that a widget's font can't hold and this
/// build can't draw (it has no Thai face, or the face lacks a character), which would come out
/// as `?`. `None` for text it can draw, and for a password field, which shows `*`.
pub(crate) fn thai_refusal(doc: &Document, f: &Field, text: &str) -> Option<String> {
    if f.has(flags::PASSWORD) || !text.chars().any(shaped_text::is_thai) {
        return None;
    }
    let held = |w: &Widget| {
        let wd = doc.get(w.obj).as_dict().cloned().unwrap_or_default();
        let da = parse_da(wd.get(b"DA").and_then(|o| doc.resolve(o).as_string().map(|s| s.to_text())).as_deref().unwrap_or(&f.da));
        dr_font(doc, &da.font).is_some_and(|(_, enc)| matches!(enc, FontText::Simple(_)) && text.split(['\n', '\r']).all(|l| enc.can_encode(l)))
    };
    if f.widgets.iter().all(held) { None } else { shaped_text::thai_refusal(text) }
}

/// The values a field shows: with `format`, a text field's or combo box's single value as its
/// Format event shows it, not as it is stored.
fn shown_values<'a>(doc: &Document, f: &Field, wd: &Dict, values: &'a [String], format: bool) -> std::borrow::Cow<'a, [String]> {
    match values {
        [value] if format && matches!(f.kind, crate::FieldKind::Text | crate::FieldKind::Combo) && f.actions.format != crate::af::Format::None => {
            let shown = f.actions.format_value(value);
            // Month and weekday names in the date language that no font here can draw ("окт." in
            // Helvetica) are drawn in English rather than as `?` (#5303).
            let english = || f.actions.format_value_in(value, &crate::af::ENGLISH_DATE_NAMES);
            let dated = matches!(f.actions.format, crate::af::Format::Date(_) | crate::af::Format::Time(_));
            std::borrow::Cow::Owned(vec![if !dated || draws(doc, f, wd, &shown) { shown } else { english() }])
        }
        _ => std::borrow::Cow::Borrowed(values),
    }
}

/// [`field_appearance_drawn`], and whether the text drawn is cut off by the box.
fn field_appearance_fit(doc: &Document, f: &Field, w: &Widget, values: &[String], format: bool) -> (Stream, Option<char>, Option<Overflow>) {
    let wobj = doc.get(w.obj);
    let wd = wobj.as_dict().cloned().unwrap_or_default();
    let shown = shown_values(doc, f, &wd, values, format);
    let values: &[String] = &shown;
    let (page_w, page_h) = ((w.rect[2] - w.rect[0]).max(1.0), (w.rect[3] - w.rect[1]).max(1.0));
    let place = placement(mk_rotation(doc, &wd), page_w, page_h);
    let (width, height) = (place.layout_w, place.layout_h);
    let da = parse_da(wd.get(b"DA").and_then(|o| doc.resolve(o).as_string().map(|s| s.to_text())).as_deref().unwrap_or(&f.da));
    let text: String = if f.kind == FieldKind::Combo {
        values.iter().map(|v| f.options.iter().find(|(e, _)| e == v).map_or(v.clone(), |(_, d)| d.clone())).collect::<Vec<_>>().join(", ")
    } else {
        values.first().cloned().unwrap_or_default()
    };
    let text = if f.has(flags::PASSWORD) { "*".repeat(text.chars().count()) } else { text };
    let can_show = |enc: &FontText, text: &str| {
        if f.kind == FieldKind::List {
            f.options.iter().all(|(_, display)| enc.can_encode(display))
        } else if f.kind == FieldKind::Text && f.has(flags::MULTILINE) {
            text.split(['\n', '\r']).all(|line| enc.can_encode(line))
        } else {
            enc.can_encode(text)
        }
    };
    let dr = dr_font(doc, &da.font);
    // Thai the form's font can't hold (it isn't a simple font encoding it) is drawn from the Thai
    // face, as Add text draws it: Type 3 glyphs beside the form's font, which draws the rest. One
    // line of a text field or combo box at a time; the `/DA` is kept.
    let thai = matches!(f.kind, FieldKind::Text | FieldKind::Combo)
        && !f.is_comb()
        && text.chars().any(shaped_text::is_thai)
        && !dr.as_ref().is_some_and(|(_, enc)| matches!(enc, FontText::Simple(_)) && can_show(enc, &text))
        && shaped_text::thai_refusal(&text).is_none();
    // Symbols no standard font has (☐ ✓ ★) are drawn the same way from the craft-fonts symbol
    // faces (#10), unless the form's own font holds the text.
    let shaped = thai || symbols_drawn(f, dr.as_ref().is_some_and(|(_, enc)| matches!(enc, FontText::Simple(_)) && can_show(enc, &text)), &text);
    let latin_text: String =
        if shaped { text.chars().filter(|c| !shaped_text::is_thai(*c) && !shaped_text::is_fallback_symbol(*c)).collect() } else { text.clone() };
    let (font_name, font_obj, enc) = match dr.filter(|(_, enc)| can_show(enc, &latin_text)) {
        Some((o, enc)) => (da.font.clone(), o, enc),
        None => ("Helv".to_string(), helvetica(), FontText::WinAnsi),
    };
    // A form font the file doesn't embed is drawn with its open twin, which has its widths (#14).
    let twin = (font_name == da.font).then(|| form_font_twin(doc, &font_obj)).flatten();
    let width_of = |text: &str, size: f64| measure(text, size, &enc, shaped, twin.as_ref().map(|t| t.0));
    // How far a line's ink reaches below and above its baseline (em), where Thai stacks or
    // descenders reach past the form font's (Helvetica's) -0.207 and 0.718; `None` otherwise.
    let thai_ink = |line: &str| thai.then(|| shaped_text::thai_reach(line)).flatten().map(|[lo, hi]| [lo.min(-0.207), hi.max(0.718)]);
    let (mut c, bw) = frame(doc, &wd, width, height);
    let pad = 2.0 + bw;
    let inner_w = (width - 2.0 * pad).max(1.0);
    let q = wd.get(b"Q").and_then(|o| doc.resolve(o).as_int()).unwrap_or(f.quadding);
    // A rich text value (`/RV`) for this text is drawn with its own fonts, sizes and colours (#3);
    // one this can't draw (a script beyond the standard fonts, malformed or stale XHTML) falls
    // through to the plain value below.
    if let Some(drawn) = rich_field_appearance(doc, f, &da, &text, &c, Box2 { width, height, bw, pad, q }, place) {
        return drawn;
    }
    // The lines to draw (where, and what), drawn once all are known: their Thai shares fonts.
    let mut shown: Vec<(f64, f64, String)> = Vec::new();
    let mut show = |x: f64, y: f64, text: &str| {
        // A line break in a single-line value is a space, as in viewers (multiline text is split).
        shown.push((x, y, text.chars().map(|c| if matches!(c, '\n' | '\r') { ' ' } else { c }).collect()));
    };
    let x_for = |text: &str, size: f64| -> f64 {
        let tw = width_of(text, size);
        match q {
            1 => pad + (inner_w - tw) / 2.0,
            2 => width - pad - tw,
            _ => pad,
        }
    };
    let mut size = da.size;
    let mut overflow = None;
    match f.kind {
        FieldKind::List => {
            // Every option, one per line from the top (from `/TI`); selected ones highlighted.
            let fobj = doc.get(f.obj);
            let rows = list_rows_of(doc, &f.da, fobj.as_dict(), &wd);
            size = rows.size;
            let line = rows.row;
            let mut y = height - rows.inset;
            // The field's own selection (its `/I`) when these are its values; values it doesn't
            // hold yet select by export value alone.
            let picked = if values == f.value.as_slice() { f.selected.clone() } else { crate::selected_options(&f.options, values, None) };
            for (i, (_, display)) in f.options.iter().enumerate().skip(rows.top) {
                if y - line < 0.0 {
                    break;
                }
                // Both sources list positions in ascending order.
                if picked.binary_search(&i).is_ok() {
                    c.push_str(&format!("0.6 0.75 0.86 rg\n{} {} {} {} re f\n", n(bw), n(y - line), n(width - 2.0 * bw), n(line)));
                }
                show(pad, y - line + size * 0.25, display);
                y -= line;
            }
        }
        _ => {
            let comb = f.is_comb();
            if f.kind == FieldKind::Text && f.has(flags::MULTILINE) {
                if size == 0.0 {
                    size = auto_text_size(&text, inner_w, height - 2.0 * pad, true, width_of);
                }
                let lines = wrap_with(&text, size, inner_w, width_of);
                // The first line hangs from the top by its ascent: higher for stacked Thai marks.
                let mut y = height - pad - size * lines.first().and_then(|l| thai_ink(l)).map_or(0.85, |[_, hi]| hi.max(0.85));
                // The lines whose descent stays inside the border; the rest are cut off.
                let mut room = 0;
                for line in &lines {
                    if y < -size {
                        break;
                    }
                    let descent = thai_ink(line).map_or(0.207, |[lo, _]| -lo);
                    if y - descent * size >= bw - 0.01 {
                        room += 1;
                    }
                    show(x_for(line, size), y, line);
                    y -= size * 1.15;
                }
                if room < lines.len() {
                    overflow = Some(Overflow::Tall { lines: lines.len(), room, size });
                } else if let Some(tw) = lines.iter().map(|l| width_of(l, size)).filter(|tw| *tw > inner_w + 0.01).reduce(f64::max) {
                    // A character wider than the box is a line of its own, still cut at the sides.
                    overflow = Some(Overflow::Wide { width: tw, room: inner_w, size });
                }
            } else {
                if size == 0.0 {
                    size = if comb {
                        comb_auto_text_size(height - 2.0 * pad)
                    } else {
                        auto_text_size(&text, inner_w, height - 2.0 * pad, false, width_of)
                    };
                }
                // Centre Helvetica's ascent (0.718) and descent (0.207) vertically, or the reach of
                // Thai stacks and descenders, which an automatic size also keeps inside the padding.
                let reach = thai_ink(&text);
                if let Some([lo, hi]) = reach
                    && da.size == 0.0
                {
                    size = size.min((height - 2.0 * pad) / (hi - lo)).max(4.0);
                }
                let (y, ink) = match reach {
                    Some([lo, hi]) => ((height - (hi - lo) * size) / 2.0 - lo * size, (hi - lo) * size),
                    None => ((height - 0.925 * size) / 2.0 + 0.207 * size, 0.925 * size),
                };
                // The clip box is inside the border; a line taller than it is cut at the top and bottom.
                let room_h = height - 2.0 * bw;
                if ink > room_h + 0.01 {
                    overflow = Some(Overflow::High { height: ink, room: room_h, size });
                }
                if comb {
                    let cells = f.max_len.unwrap_or(1).max(1);
                    let cell = width / cells as f64;
                    c.push_str(&comb_dividers(doc, &wd, width, height, cells));
                    for (i, ch) in text.chars().take(cells).enumerate() {
                        let s = ch.to_string();
                        show(cell * i as f64 + (cell - width_of(&s, size)) / 2.0, y, &s);
                    }
                } else {
                    let tw = width_of(&text.replace(['\n', '\r'], " "), size);
                    if overflow.is_none() && tw > inner_w + 0.01 {
                        overflow = Some(Overflow::Wide { width: tw, room: inner_w, size });
                    }
                    show(x_for(&text, size), y, &text);
                }
            }
        }
    }
    let (body, fonts, unencodable) = draw_lines(&shown, size, &font_name, font_obj, &enc, shaped, twin.as_ref().map(|t| t.0));
    // Acrobat regenerates only what is inside /Tx BMC when it redraws a field (e.g. on focus
    // changes), discarding what came before it (issues #1613, #633). The background and border
    // therefore go inside the marked-content block too, so a redraw keeps them with the text.
    let mut content = format!(
        "/Tx BMC\nq\n{c}{} {} {} {} re W n\nBT\n/{} {} Tf\n{}\n",
        n(bw),
        n(bw),
        n(width - 2.0 * bw),
        n(height - 2.0 * bw),
        font_name,
        n(size),
        da.color
    )
    .into_bytes();
    content.extend(body);
    content.extend_from_slice(b"ET\nQ\nEMC\n");
    let mut res = Dict::new();
    res.set(b"Font".to_vec(), Object::Dict(fonts));
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("XObject"));
    d.set(b"Subtype".to_vec(), Object::name("Form"));
    set_form_box(&mut d, place);
    d.set(b"Resources".to_vec(), Object::Dict(res));
    mark_twins(&mut d, twin.iter().map(|(script, base)| (base.as_str(), *script)));
    // Nothing drawn is nothing cut, however small the box.
    let overflow = overflow.filter(|_| !text.is_empty());
    (Stream::flate(d, &content), unencodable, overflow)
}

/// The width of `text` at `size` as the plain value draws it: in the field's font `enc`, with Thai
/// and fallback symbols from the craft-fonts faces when `shaped`, or with the field font's open
/// `twin` (#14).
fn measure(text: &str, size: f64, enc: &FontText, shaped: bool, twin: Option<ShapedScript>) -> f64 {
    match twin {
        Some(twin) => shaped_text::twin_width(text, size, twin, |s, size| enc.width(s, size)),
        None if shaped => shaped_text::fallback_width(text, size, |s, size| enc.width(s, size)),
        None => enc.width(text, size),
    }
}

/// The key on an appearance PdfCraft drew with open twins (#14): `Calibri|Carlito`, one pair per
/// font, `; ` between them. The engine reads it to say so; the form's own fonts are unchanged.
pub const TWIN_MARK: &[u8] = b"PCTwin";

/// Record on the appearance `d` which form fonts (`/BaseFont`) were drawn with which twin.
fn mark_twins<'a>(d: &mut Dict, twins: impl Iterator<Item = (&'a str, ShapedScript)>) {
    let mut pairs: Vec<String> = twins.filter_map(|(base, t)| Some(format!("{base}|{}", pdfcraft_fonts::twin_label(t)?))).collect();
    pairs.sort();
    pairs.dedup();
    if !pairs.is_empty() {
        d.set(TWIN_MARK.to_vec(), Object::String(pdfcraft_cos::PdfString::text(&pairs.join("; "))));
    }
}

/// The open twin of the form font `font` (a `/DR` entry) and its `/BaseFont`, when the file
/// doesn't embed it (#14): a simple font whose descriptor has no font program, and a twin in the
/// craft-fonts Latin faces for its family in its weight and posture (from the name).
fn form_font_twin(doc: &Document, font: &Object) -> Option<(ShapedScript, String)> {
    let font = doc.resolve(font);
    let fd = font.as_dict()?;
    if !matches!(fd.name(b"Subtype"), Some(b"Type1" | b"TrueType" | b"MMType1")) {
        return None;
    }
    let embedded = fd
        .get(b"FontDescriptor")
        .map(|d| doc.resolve(d))
        .and_then(|d| d.as_dict().map(|d| [&b"FontFile"[..], b"FontFile2", b"FontFile3"].iter().any(|k| d.contains(k))))
        .unwrap_or(false);
    if embedded {
        return None;
    }
    let base = String::from_utf8_lossy(fd.name(b"BaseFont")?).into_owned();
    let style = base.split_once(['-', ',']).map_or(String::new(), |(_, s)| s.to_ascii_lowercase());
    let bold = ["bold", "black", "heavy"].iter().any(|w| style.contains(w));
    let italic = style.contains("italic") || style.contains("oblique");
    Some((pdfcraft_fonts::twin_for(&base, bold, italic)?, base))
}

/// Whether a text field or combo box draws the fallback symbols in `text` from the craft-fonts
/// symbol faces: it has some, it isn't a comb field, and the form's own font doesn't hold the text
/// (`own_font_holds`).
fn symbols_drawn(f: &Field, own_font_holds: bool, text: &str) -> bool {
    matches!(f.kind, FieldKind::Text | FieldKind::Combo) && !f.is_comb() && !own_font_holds && text.chars().any(shaped_text::is_fallback_symbol)
}

/// The text operators for the `shown` lines (where each starts, and what it says) at `size`, in
/// the field's font `font_name` (`font`, written with `enc`) and, with `thai`, their Thai and
/// fallback symbols from the craft-fonts faces; the font resources they use, and the first character drawn as `?` because the
/// font can't hold it, for the caller to report.
fn draw_lines(
    shown: &[(f64, f64, String)],
    size: f64,
    font_name: &str,
    font: Object,
    enc: &FontText,
    thai: bool,
    twin: Option<ShapedScript>,
) -> (Vec<u8>, Dict, Option<char>) {
    let mut fonts = Dict::new();
    fonts.set(font_name.as_bytes().to_vec(), font);
    let lines: Vec<String> = shown.iter().map(|(_, _, text)| text.clone()).collect();
    let (pieces, shaped) = if thai || twin.is_some() {
        shaped_text::appearance_lines_in(&lines, &mut fonts, twin)
    } else {
        (lines.into_iter().map(|l| vec![Piece::Latin(l)]).collect(), ShapedFonts::default())
    };
    let mut body = Vec::new();
    let mut current = None;
    for ((x, y, _), pieces) in shown.iter().zip(&pieces) {
        shaped.show_line(&mut body, pieces, *x, *y, size, font_name, |s| enc.encode(s), |s| enc.width(s, size), &mut current);
    }
    let unencodable = pieces.iter().flatten().find_map(|p| match p {
        Piece::Latin(s) => enc.first_unencodable(s),
        Piece::Shaped(_) => None,
    });
    (body, fonts, unencodable)
}

/// A field's own `key`, else its nearest ancestor's (`/RV` and `/DS` are inheritable, §12.7.4.3),
/// as text: a text string, or a stream's bytes.
fn inherited_text(doc: &Document, field: pdfcraft_cos::ObjRef, key: &[u8]) -> Option<String> {
    let mut at = Some(field);
    let mut seen = std::collections::HashSet::new();
    while let Some(r) = at {
        if seen.len() > 64 || !seen.insert(r) {
            return None;
        }
        let d = doc.get(r).as_dict().cloned()?;
        if let Some(v) = d.get(key) {
            return match &*doc.resolve(v) {
                Object::String(s) => Some(s.to_text()),
                Object::Stream(s) => s.decoded().ok().map(|b| String::from_utf8_lossy(&b).into_owned()),
                _ => None,
            };
        }
        at = d.reference(b"Parent");
    }
    None
}

/// The rich text value to draw for `text` in a text field set to rich text (§12.7.4.3): its
/// `/RV` over the style of its `/DA` and `/DS` (the field's, else the form's). `None` for a field
/// drawn plain: not rich text, a comb or password field, no `/RV` that can be read, or a `/RV`
/// for some other value.
fn rich_value(doc: &Document, f: &Field, da: &Da, text: &str) -> Option<RichValue> {
    if f.kind != FieldKind::Text || !f.has(flags::RICH_TEXT) || f.is_comb() || f.has(flags::PASSWORD) {
        return None;
    }
    let rv = inherited_text(doc, f.obj, b"RV")?;
    let ds = inherited_text(doc, f.obj, b"DS").or_else(|| acroform(doc)?.get(b"DS").and_then(|o| doc.resolve(o).as_string().map(|s| s.to_text())));
    // The `/DA` font's own name tells its family and style (a `/DR` font named `/F1` is
    // `/Helvetica-Bold` underneath).
    let font_name = acroform(doc)
        .and_then(|af| {
            let dr = doc.resolve(af.get(b"DR")?);
            let fonts = doc.resolve(dr.as_dict()?.get(b"Font")?);
            let font = doc.resolve(fonts.as_dict()?.get(da.font.as_bytes())?);
            font.as_dict()?.name(b"BaseFont").map(|b| String::from_utf8_lossy(b).into_owned())
        })
        .unwrap_or_else(|| da.font.clone());
    let base = Style::of_font(&font_name, da.size, rich::da_rgb(&da.color));
    rich::parse(&rv, ds.as_deref(), base).filter(|r| r.matches(text))
}

/// A rich text field's appearance (#3), when its `/RV` can be drawn: see [`rich_value`] and
/// [`rich_appearance`]. `None` draws the plain value.
fn rich_field_appearance(
    doc: &Document,
    f: &Field,
    da: &Da,
    text: &str,
    frame: &str,
    b: Box2,
    place: Placement,
) -> Option<(Stream, Option<char>, Option<Overflow>)> {
    let rich = rich_value(doc, f, da, text)?;
    // Text the standard fonts can't hold is drawn plain where plain drawing has a font for it:
    // Thai (its shaped face) and the form's own composite font (CJK forms). Anywhere else the
    // plain value would show the same `?` and lose the formatting too, so the rich value is drawn
    // with `?` for it and the character is reported, as the plain value's would be.
    let undrawable =
        text.chars().any(|c| !shaped_text::is_fallback_symbol(c) && pdfcraft_fonts::first_undrawable(c.encode_utf8(&mut [0; 4])).is_some());
    if undrawable && (text.chars().any(shaped_text::is_thai) || matches!(dr_font(doc, &da.font), Some((_, FontText::Unicode(_))))) {
        return None;
    }
    let drawn = rich_appearance(doc, &rich, frame, b, f.has(flags::MULTILINE));
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("XObject"));
    d.set(b"Subtype".to_vec(), Object::name("Form"));
    set_form_box(&mut d, place);
    let mut res = Dict::new();
    res.set(b"Font".to_vec(), Object::Dict(drawn.fonts));
    d.set(b"Resources".to_vec(), Object::Dict(res));
    mark_twins(&mut d, drawn.twins.iter().map(|(base, t)| (base.as_str(), *t)));
    Some((Stream::flate(d, &drawn.content), drawn.undrawn, drawn.overflow))
}

/// The box text is drawn in: its size, border width and padding, and its quadding (`/Q`).
#[derive(Clone, Copy)]
struct Box2 {
    width: f64,
    height: f64,
    bw: f64,
    pad: f64,
    q: i64,
}

/// A rich value's appearance stream content, the fonts it uses, whether it is cut off, and the
/// first character drawn as `?` because no standard font holds it.
struct RichDrawn {
    content: Vec<u8>,
    fonts: Dict,
    overflow: Option<Overflow>,
    undrawn: Option<char>,
    /// The form fonts (`/BaseFont`) drawn with an open twin, and the twin (#14).
    twins: Vec<(String, ShapedScript)>,
}

/// The font a rich run is drawn in: a standard 14 font, or one of the form's own (`/DR`, #9).
enum RunFont {
    Standard(pdfcraft_fonts::Std14),
    Form {
        resource: String,
        object: Object,
        text: FontText,
    },
    /// A form font the file doesn't embed (`base`), drawn with its open twin (#14); characters the
    /// twin lacks are drawn in the standard font `standard`.
    Twin {
        twin: ShapedScript,
        standard: pdfcraft_fonts::Std14,
        base: String,
    },
}

impl RunFont {
    /// The width of `s` as it is drawn: its fallback symbols from the symbol faces (#10), the
    /// rest in this font.
    fn width(&self, s: &str, size: f64) -> f64 {
        match self {
            RunFont::Twin { twin, standard, .. } => shaped_text::twin_width(s, size, *twin, |s, size| standard.text_width(s, size)),
            _ => shaped_text::fallback_width(s, size, |s, size| self.own_width(s, size)),
        }
    }

    /// `s` as this font draws it: a twin's glyphs and the rest, or fallback symbols and the rest.
    fn pieces(&self, s: &str) -> Vec<Piece> {
        let own = || vec![Piece::Latin(s.to_string())];
        match self {
            RunFont::Twin { twin, .. } => shaped_text::twin_pieces(s, *twin),
            _ if shaped_text::has_fallback(s) => shaped_text::fallback_pieces(s).unwrap_or_else(|_| own()),
            _ => own(),
        }
    }

    /// The width of `s` in this font alone.
    fn own_width(&self, s: &str, size: f64) -> f64 {
        match self {
            RunFont::Standard(f) | RunFont::Twin { standard: f, .. } => f.text_width(s, size),
            RunFont::Form { text, .. } => text.width(&spaces_as(text, s), size),
        }
    }

    fn encode(&self, s: &str) -> Vec<u8> {
        match self {
            RunFont::Standard(_) | RunFont::Twin { .. } => win_ansi(s),
            RunFont::Form { text, .. } => text.encode(&spaces_as(text, s)),
        }
    }

    /// Its resource name in the appearance, and the font object there: a form font as `/DR`
    /// holds it (never copied or embedded anew), a standard one under Acrobat's name.
    fn resource(&self) -> (String, Object) {
        match self {
            RunFont::Standard(f) | RunFont::Twin { standard: f, .. } => {
                let mut fd = Dict::new();
                fd.set(b"Type".to_vec(), Object::name("Font"));
                fd.set(b"Subtype".to_vec(), Object::name("Type1"));
                fd.set(b"BaseFont".to_vec(), Object::name(f.base_font()));
                fd.set(b"Encoding".to_vec(), Object::name("WinAnsiEncoding"));
                (crate::FieldFont::from_resource(f.base_font()).map_or("Helv", |f| f.resource()).to_string(), Object::Dict(fd))
            }
            RunFont::Form { resource, object, .. } => (resource.clone(), object.clone()),
        }
    }
}

/// `s` with each space character the form font can't encode (a no-break space, `&#160;` in
/// Acrobat's rich text) as a plain space, which it draws the same.
fn spaces_as(font: &FontText, s: &str) -> String {
    s.chars().map(|c| if c.is_whitespace() && c != ' ' && !font.can_encode(c.encode_utf8(&mut [0; 4])) { ' ' } else { c }).collect()
}

/// A font family name as it compares between a run and a `/BaseFont`: letters and digits only,
/// lower case, without PostScript's `PS`/`MT` endings (`Times New Roman` is `TimesNewRomanPSMT`).
fn family_key(name: &str) -> String {
    let key: String = name.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_ascii_lowercase();
    let key = key.strip_suffix("mt").unwrap_or(&key);
    key.strip_suffix("ps").unwrap_or(key).to_string()
}

/// The form's `/DR` fonts as faces: resource name, family key, bold, italic, read from each
/// font's `/BaseFont` (`ABCDEF+Calibri-BoldItalic`, `Arial,Bold`).
fn form_faces(doc: &Document) -> Vec<(String, String, bool, bool)> {
    let fonts = acroform(doc).and_then(|af| {
        let dr = doc.resolve(af.get(b"DR")?);
        doc.resolve(dr.as_dict()?.get(b"Font")?).as_dict().cloned()
    });
    let Some(fonts) = fonts else { return Vec::new() };
    fonts
        .iter()
        .filter_map(|(key, entry)| {
            let base = doc.resolve(entry).as_dict()?.name(b"BaseFont").map(|b| String::from_utf8_lossy(b).into_owned())?;
            let base = base.split_once('+').filter(|(tag, _)| tag.len() == 6).map_or(base.as_str(), |(_, rest)| rest).to_string();
            let (family, style) = base.split_once(['-', ',']).unwrap_or((&base, ""));
            let style = style.to_ascii_lowercase();
            let bold = ["bold", "black", "heavy"].iter().any(|w| style.contains(w));
            let italic = style.contains("italic") || style.contains("oblique");
            Some((String::from_utf8_lossy(key).into_owned(), family_key(family), bold, italic))
        })
        .collect()
}

/// The font for a run in `style` showing `text`: the form's font of the face it names in that
/// weight and posture, when the form has one that holds every character of the text; else the
/// standard 14 font, which keeps the weight and posture (#9: a bold run with only a regular form
/// font is drawn in Helvetica-Bold, not in the regular face).
fn run_font(doc: &Document, faces: &[(String, String, bool, bool)], style: &Style, text: &str) -> RunFont {
    let standard = style.font();
    // A face the form names but doesn't embed is drawn with its open twin (#14): the `/DR` font's
    // own, or, with no such font in `/DR`, the twin of the name the run gives.
    let twin =
        |base: &str| pdfcraft_fonts::twin_for(base, style.bold, style.italic).map(|twin| RunFont::Twin { twin, standard, base: base.to_string() });
    let form = style.face.and_then(|face| {
        let want = family_key(face.as_str());
        let Some((resource, ..)) = faces.iter().find(|(_, family, bold, italic)| *family == want && *bold == style.bold && *italic == style.italic)
        else {
            return twin(face.as_str());
        };
        let (object, font) = dr_font(doc, resource)?;
        if let Some((twin, base)) = form_font_twin(doc, &object) {
            return Some(RunFont::Twin { twin, standard, base });
        }
        // Fallback symbols are drawn from the symbol faces, whatever font the run is in.
        let own: String = text.chars().filter(|c| !shaped_text::is_fallback_symbol(*c)).collect();
        font.draws(&spaces_as(&font, &own.replace('\n', " "))).then(|| RunFont::Form { resource: resource.clone(), object, text: font })
    });
    form.unwrap_or(RunFont::Standard(standard))
}

/// One laid-out line: its runs (font, style, text), how wide it is and its tallest size.
struct RichLine {
    runs: Vec<(usize, Style, String)>,
    width: f64,
    size: f64,
    align: Option<rich::Align>,
}

/// Lines of `rich` at `size_of` each style, each span in its font (`fonts`, by `font_of` the
/// span's paragraph and position), wrapped to `room` when `wrap` (a multiline field), else one
/// line, as a single-line field shows paragraphs and line breaks as spaces.
fn rich_lines(rich: &RichValue, fonts: &[RunFont], font_of: &[Vec<usize>], size_of: &dyn Fn(&Style) -> f64, room: f64, wrap: bool) -> Vec<RichLine> {
    let new_line = |align| RichLine { runs: Vec::new(), width: 0.0, size: 0.0, align };
    let mut lines = Vec::new();
    let mut line = new_line(rich.paragraphs.first().and_then(|p| p.align));
    for (i, p) in rich.paragraphs.iter().enumerate() {
        let span_fonts = font_of.get(i).map_or(&[][..], Vec::as_slice);
        if i > 0 {
            if wrap {
                lines.push(std::mem::replace(&mut line, new_line(p.align)));
            } else if let (Some(first), Some(&f)) = (p.spans.first(), span_fonts.first()) {
                line.runs.push((f, first.style, " ".into()));
            }
        }
        for (span, &f) in p.spans.iter().zip(span_fonts) {
            let size = size_of(&span.style);
            let Some(font) = fonts.get(f) else { continue };
            for (j, part) in span.text.split('\n').enumerate() {
                if j > 0 {
                    if wrap {
                        lines.push(std::mem::replace(&mut line, new_line(p.align)));
                    } else {
                        line.runs.push((f, span.style, " ".into()));
                    }
                }
                // Words with the spaces after them, so a line breaks after its spaces.
                for word in part.split_inclusive(' ') {
                    let ink = font.width(word.trim_end_matches(' '), size);
                    // The line so far with the spaces it ends in, which come before this word.
                    let so_far: f64 = line.runs.iter().filter_map(|(f, s, t)| Some(fonts.get(*f)?.width(t, size_of(s)))).sum();
                    if wrap && !line.runs.is_empty() && so_far + ink > room + 0.01 {
                        lines.push(std::mem::replace(&mut line, new_line(p.align)));
                    }
                    // A wrapped line doesn't start with the spaces that ended the one before.
                    if line.runs.is_empty() && word.trim().is_empty() && !lines.is_empty() {
                        continue;
                    }
                    match line.runs.last_mut() {
                        Some((font, style, text)) if *font == f && *style == span.style => text.push_str(word),
                        _ => line.runs.push((f, span.style, word.to_string())),
                    }
                    line.size = line.size.max(size);
                }
            }
            line.size = line.size.max(size);
        }
    }
    lines.push(line);
    for l in &mut lines {
        l.width = line_ink(l, fonts, size_of);
    }
    lines
}

/// How wide a line's ink is: its runs, without the spaces it ends in.
fn line_ink(line: &RichLine, fonts: &[RunFont], size_of: &dyn Fn(&Style) -> f64) -> f64 {
    let last = line.runs.len().saturating_sub(1);
    line.runs
        .iter()
        .enumerate()
        .filter_map(|(i, (f, style, text))| Some(fonts.get(*f)?.width(if i == last { text.trim_end_matches(' ') } else { text }, size_of(style))))
        .sum()
}

/// Draw `rich` in the box `b` (inside the frame `frame` already drawn), aligned by its `/Q` where
/// a paragraph doesn't say, on wrapped lines when `multiline`. Each run is drawn in the form's own
/// font where it has one for it, else a standard 14 font, in which a character no standard font
/// holds is drawn and measured as `?`.
fn rich_appearance(doc: &Document, rich: &RichValue, frame: &str, b: Box2, multiline: bool) -> RichDrawn {
    let faces = form_faces(doc);
    let mut fonts: Vec<RunFont> = Vec::new();
    let font_of: Vec<Vec<usize>> = rich
        .paragraphs
        .iter()
        .map(|p| {
            p.spans
                .iter()
                .map(|s| {
                    fonts.push(run_font(doc, &faces, &s.style, &s.text));
                    fonts.len() - 1
                })
                .collect()
        })
        .collect();
    let runs = || rich.paragraphs.iter().zip(&font_of).flat_map(|(p, f)| p.spans.iter().zip(f));
    // What the standard fonts draw as `?`: in standard runs, and in twin runs what the twin lacks.
    let undrawn = runs().find_map(|(s, f)| {
        let font = fonts.get(*f)?;
        let drawn_elsewhere = |c: char| match font {
            RunFont::Twin { twin, .. } => pdfcraft_fonts::face_cluster(*twin, c).is_some(),
            RunFont::Standard(_) => false,
            RunFont::Form { .. } => true,
        };
        let own: String = s.text.chars().filter(|c| !shaped_text::is_fallback_symbol(*c) && !drawn_elsewhere(*c)).collect();
        pdfcraft_fonts::first_undrawable(&own.replace('\n', " "))
    });
    let twins: Vec<(String, ShapedScript)> = fonts
        .iter()
        .filter_map(|f| match f {
            RunFont::Twin { twin, base, .. } => Some((base.clone(), *twin)),
            _ => None,
        })
        .collect();
    let inner_w = (b.width - 2.0 * b.pad).max(1.0);
    let inner_h = (b.height - 2.0 * b.pad).max(1.0);
    // Runs without a size of their own (0, automatic) take one that fits the plain text in the
    // first run's font, as the plain value would be sized.
    let auto = match runs().find(|(s, _)| s.style.size == 0.0).and(runs().next()).and_then(|(_, f)| fonts.get(*f)) {
        Some(first) => auto_text_size(&rich.plain_text(), inner_w, inner_h, multiline, |s, size| first.width(s, size)),
        None => 12.0,
    };
    let size_of = |s: &Style| if s.size > 0.0 { s.size } else { auto };
    let lines = rich_lines(rich, &fonts, &font_of, &size_of, inner_w, multiline);
    let mut resources = Dict::new();
    // Each run's pieces: its fallback symbols as glyphs of the symbol faces' Type 3 fonts (#10),
    // the rest in the run's font. Symbols whose outlines can't be read are left to the run's font,
    // which draws `?` for them.
    let own = |t: &String| vec![shaped_text::Piece::Latin(t.clone())];
    let mut pieces: Vec<Vec<Vec<shaped_text::Piece>>> =
        lines.iter().map(|l| l.runs.iter().map(|(f, _, t)| fonts.get(*f).map_or_else(|| own(t), |font| font.pieces(t))).collect()).collect();
    let shaped = match ShapedFonts::new(
        shaped_text::clusters(pieces.iter().flatten().map(Vec::as_slice)),
        shaped_glyph_of,
        &Dict::new(),
        &mut resources,
        "PCSy",
        &mut |o| o,
    ) {
        Ok(shaped) => shaped,
        Err(_) => {
            pieces = lines.iter().map(|l| l.runs.iter().map(|(_, _, t)| own(t)).collect()).collect();
            ShapedFonts::default()
        }
    };
    let mut body: Vec<u8> = Vec::new();
    let mut underlines = String::new();
    let mut overflow = None;
    let mut y = if multiline {
        b.height - b.pad - 0.85 * lines.first().map_or(auto, |l| l.size)
    } else {
        let s = lines.first().map_or(auto, |l| l.size);
        if 0.925 * s > b.height - 2.0 * b.bw + 0.01 {
            overflow = Some(Overflow::High { height: 0.925 * s, room: b.height - 2.0 * b.bw, size: s });
        }
        (b.height - 0.925 * s) / 2.0 + 0.207 * s
    };
    let mut room = 0;
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            y -= 1.15 * line.size;
        }
        if y - 0.207 * line.size >= b.bw - 0.01 {
            room += 1;
        }
        if overflow.is_none() && line.width > inner_w + 0.01 {
            overflow = Some(Overflow::Wide { width: line.width, room: inner_w, size: line.size });
        }
        let align = line.align.unwrap_or(match b.q {
            1 => rich::Align::Center,
            2 => rich::Align::Right,
            _ => rich::Align::Left,
        });
        let mut x = match align {
            rich::Align::Left => b.pad,
            rich::Align::Center => b.pad + (inner_w - line.width) / 2.0,
            rich::Align::Right => b.width - b.pad - line.width,
        };
        let mut current = None;
        for ((f, style, text), run_pieces) in line.runs.iter().zip(pieces.get(i).map_or(&[][..], Vec::as_slice)) {
            let Some(font) = fonts.get(*f) else { continue };
            let size = size_of(style);
            let (resource, object) = font.resource();
            if !resources.contains(resource.as_bytes()) {
                resources.set(resource.as_bytes().to_vec(), object);
            }
            let [r, g, bl] = style.color;
            let rgb = format!("{} {} {} rg", n(r), n(g), n(bl));
            body.extend_from_slice(format!("{rgb}\n").as_bytes());
            let start = x;
            for piece in run_pieces {
                match piece {
                    shaped_text::Piece::Latin(t) => {
                        body.extend_from_slice(format!("/{resource} {} Tf\n1 0 0 1 {} {} Tm\n", n(size), n(x), n(y)).as_bytes());
                        // The font's own codes (WinAnsi bytes for a standard font), not UTF-8.
                        body.extend(literal(&font.encode(t)));
                        body.extend_from_slice(b" Tj\n");
                        x += font.own_width(t, size);
                        current = None;
                    }
                    shaped_text::Piece::Shaped(clusters) => x = shaped.show(&mut body, clusters, x, y, size, 0.0, 0.0, &mut current),
                }
            }
            if style.underline {
                let ink = font.width(text.trim_end_matches(' '), size);
                underlines.push_str(&format!("{rgb}\n{} {} {} {} re f\n", n(start), n(y - 0.12 * size), n(ink), n((0.05 * size).max(0.5))));
            }
        }
    }
    if multiline && room < lines.len() {
        overflow = Some(Overflow::Tall { lines: lines.len(), room, size: lines.iter().map(|l| l.size).fold(0.0, f64::max) });
    }
    // As for plain text: background and border inside /Tx BMC, so a viewer's redraw keeps them.
    let mut content =
        format!("/Tx BMC\nq\n{frame}{} {} {} {} re W n\nBT\n", n(b.bw), n(b.bw), n(b.width - 2.0 * b.bw), n(b.height - 2.0 * b.bw)).into_bytes();
    content.extend(body);
    content.extend_from_slice(format!("ET\n{underlines}Q\nEMC\n").as_bytes());
    RichDrawn { content, fonts: resources, overflow, undrawn, twins }
}

/// On/Off appearances for a check box or radio button that has none (a check mark or a dot,
/// drawn as paths, so no symbol font is needed).
/// The mark is drawn in the colour of the widget's `/DA`, else of `f_da` (the field's): the
/// Appearance tab's Text Color (#4261).
/// The streams are added to `doc` as indirect objects (streams can't be direct objects).
pub fn check_box_states(doc: &mut Document, w: &Widget, kind: FieldKind, on_name: &str, f_da: &str) -> Dict {
    let wobj = doc.get(w.obj);
    let wd = wobj.as_dict().cloned().unwrap_or_default();
    let (page_w, page_h) = ((w.rect[2] - w.rect[0]).max(1.0), (w.rect[3] - w.rect[1]).max(1.0));
    let place = placement(mk_rotation(doc, &wd), page_w, page_h);
    let (width, height) = (place.layout_w, place.layout_h);
    let (frame_c, _) = frame(doc, &wd, width, height);
    let style = crate::author::CheckStyle::of_widget(doc, &wd, kind);
    let da = parse_da(wd.get(b"DA").and_then(|o| doc.resolve(o).as_string().map(|s| s.to_text())).as_deref().unwrap_or(f_da));
    let mut form = |content: String| -> Object {
        let mut d = Dict::new();
        d.set(b"Type".to_vec(), Object::name("XObject"));
        d.set(b"Subtype".to_vec(), Object::name("Form"));
        set_form_box(&mut d, place);
        Object::Ref(doc.add(Object::Stream(Stream::flate(d, content.as_bytes()))))
    };
    let s = width.min(height);
    let mark = check_mark(style, width / 2.0, height / 2.0, s, &da.color);
    let mut nd = Dict::new();
    nd.set(crate::name_bytes(on_name), form(format!("{frame_c}{mark}")));
    nd.set(b"Off".to_vec(), form(frame_c));
    let mut ap = Dict::new();
    ap.set(b"N".to_vec(), Object::Dict(nd));
    ap
}

/// The on-state mark for `style` (#94), centred on (`cx`, `cy`) in a box whose smaller side is
/// `s`, drawn in the fill colour operation `color` (`0 g`, `0 0 1 rg`).
fn check_mark(style: crate::author::CheckStyle, cx: f64, cy: f64, s: f64, color: &str) -> String {
    use crate::author::CheckStyle;
    let (fill, stroke) = (color, stroke_op(color));
    // A filled polygon through `pts`.
    let polygon = |pts: &[(f64, f64)]| {
        let mut c = format!("{fill}\n");
        for (i, (x, y)) in pts.iter().enumerate() {
            c.push_str(&format!("{} {} {} ", n(*x), n(*y), if i == 0 { "m" } else { "l" }));
        }
        c.push_str("h f\n");
        c
    };
    let line = n((s * 0.1).max(1.0));
    match style {
        CheckStyle::Check => format!(
            "{stroke}\n{line} w 1 J 1 j\n{} {} m {} {} l {} {} l S\n",
            n(cx - s * 0.28),
            n(cy),
            n(cx - s * 0.08),
            n(cy - s * 0.22),
            n(cx + s * 0.3),
            n(cy + s * 0.25)
        ),
        CheckStyle::Cross => {
            let d = s * 0.25;
            format!(
                "{stroke}\n{line} w 1 J\n{} {} m {} {} l S\n{} {} m {} {} l S\n",
                n(cx - d),
                n(cy - d),
                n(cx + d),
                n(cy + d),
                n(cx - d),
                n(cy + d),
                n(cx + d),
                n(cy - d)
            )
        }
        CheckStyle::Square => {
            let d = s * 0.22;
            format!("{fill}\n{} {} {} {} re f\n", n(cx - d), n(cy - d), n(2.0 * d), n(2.0 * d))
        }
        CheckStyle::Diamond => {
            let d = s * 0.3;
            polygon(&[(cx, cy + d), (cx + d, cy), (cx, cy - d), (cx - d, cy)])
        }
        CheckStyle::Star => {
            // Five points: outer and inner radii alternate, the first point straight up.
            let (outer, inner) = (s * 0.32, s * 0.32 * 0.382);
            let pts: Vec<(f64, f64)> = (0..10)
                .map(|i| {
                    let a = std::f64::consts::FRAC_PI_2 + i as f64 * std::f64::consts::PI / 5.0;
                    let r = if i % 2 == 0 { outer } else { inner };
                    (cx + r * a.cos(), cy + r * a.sin())
                })
                .collect();
            polygon(&pts)
        }
        CheckStyle::Circle => {
            let r = s * 0.25;
            let k = 0.5523 * r;
            format!(
                "{fill}\n{} {} m {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c f\n",
                n(cx + r),
                n(cy),
                n(cx + r),
                n(cy + k),
                n(cx + k),
                n(cy + r),
                n(cx),
                n(cy + r),
                n(cx - k),
                n(cy + r),
                n(cx - r),
                n(cy + k),
                n(cx - r),
                n(cy),
                n(cx - r),
                n(cy - k),
                n(cx - k),
                n(cy - r),
                n(cx),
                n(cy - r),
                n(cx + k),
                n(cy - r),
                n(cx + r),
                n(cy - k),
                n(cx + r),
                n(cy)
            )
        }
    }
}

/// How a check box or radio button widget `w` of field `f` looks in state on (`on`) or Off when
/// the file has no appearance for that state, or leaves its appearances to the viewer
/// (`/NeedAppearances`): its `/MK` background and border ([`frame`]), and when on, its caption
/// `/MK /CA` (ISO 32000-2 Table 192: the normal caption, which check boxes and radio buttons may
/// use too) as a ZapfDingbats character, as in the check box example of
/// 12.7.5.2.3: `4` (a check mark) by default, `l` (a dot) for a radio button. The character is
/// drawn in the `/DA` colour, at the `/DA` size or, for an automatic size (`0 Tf`), filling most
/// of the box. ZapfDingbats is one of the standard 14 fonts, so no font program is needed.
pub(crate) fn button_state_shown(doc: &Document, f: &Field, w: &Widget, on: bool) -> Stream {
    let wobj = doc.get(w.obj);
    let wd = wobj.as_dict().cloned().unwrap_or_default();
    let (page_w, page_h) = ((w.rect[2] - w.rect[0]).max(1.0), (w.rect[3] - w.rect[1]).max(1.0));
    let place = placement(mk_rotation(doc, &wd), page_w, page_h);
    let (width, height) = (place.layout_w, place.layout_h);
    let (frame_c, bw) = frame(doc, &wd, width, height);
    let mut content = frame_c.into_bytes();
    let mut res = Dict::new();
    if on {
        let mk = wd.get(b"MK").map(|m| doc.resolve(m)).and_then(|m| m.as_dict().cloned());
        let caption = mk.as_ref().and_then(|mk| mk.get(b"CA")).and_then(|o| doc.resolve(o).as_string().map(|s| s.to_text()));
        let dingbats = pdfcraft_fonts::Std14::ZapfDingbats;
        // The caption's first character as a ZapfDingbats code, when the font has a glyph there.
        let code = caption
            .and_then(|s| s.chars().next())
            .and_then(|ch| u8::try_from(u32::from(ch)).ok())
            .filter(|&code| dingbats.width(code) > 0.0)
            .unwrap_or(if f.kind == FieldKind::Radio { b'l' } else { b'4' });
        let da = parse_da(wd.get(b"DA").and_then(|o| doc.resolve(o).as_string().map(|s| s.to_text())).as_deref().unwrap_or(&f.da));
        // The glyph's advance and (about) its height, in ems.
        let (advance, tall) = (dingbats.width(code) / 1000.0, 0.75);
        let room = |side: f64| (side - 2.0 * bw - 2.0).max(1.0);
        // An automatic size leaves a margin of a tenth of the room on each side.
        let size = if da.size > 0.0 { da.size } else { (0.8 * room(width) / advance.max(0.1)).min(0.8 * room(height) / tall).clamp(1.0, 300.0) };
        let (x, y) = ((width - advance * size) / 2.0, (height - tall * size) / 2.0 + 0.05 * size);
        content.extend(format!("q\nBT\n/ZaDb {} Tf\n{}\n{} {} Td\n", n(size), da.color, n(x), n(y)).bytes());
        content.extend(literal(&[code]));
        content.extend_from_slice(b" Tj\nET\nQ\n");
        let mut font = Dict::new();
        font.set(b"Type".to_vec(), Object::name("Font"));
        font.set(b"Subtype".to_vec(), Object::name("Type1"));
        font.set(b"BaseFont".to_vec(), Object::name("ZapfDingbats"));
        let mut fonts = Dict::new();
        fonts.set(b"ZaDb".to_vec(), Object::Dict(font));
        res.set(b"Font".to_vec(), Object::Dict(fonts));
    }
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("XObject"));
    d.set(b"Subtype".to_vec(), Object::name("Form"));
    set_form_box(&mut d, place);
    d.set(b"Resources".to_vec(), Object::Dict(res));
    Stream::flate(d, &content)
}

/// How a push button widget `w` of field `f` with no normal appearance looks: its `/MK`
/// background and border, in the `/BS` width and style ([`frame`]: Table 168, `B` beveled, `I`
/// inset), with its caption `/MK /CA` (Table 192) centred in the `/DA` font, size and colour.
/// A button with an icon (`/MK /I`) is drawn as PdfCraft draws its own buttons, icon included.
pub(crate) fn push_button_shown(doc: &Document, f: &Field, w: &Widget) -> Stream {
    let wobj = doc.get(w.obj);
    let wd = wobj.as_dict().cloned().unwrap_or_default();
    let mk = wd.get(b"MK").map(|m| doc.resolve(m)).and_then(|m| m.as_dict().cloned()).unwrap_or_default();
    if mk.contains(b"I") {
        return crate::author::button_appearance(doc, w, &f.da).0;
    }
    let (page_w, page_h) = ((w.rect[2] - w.rect[0]).max(1.0), (w.rect[3] - w.rect[1]).max(1.0));
    let place = placement(mk_rotation(doc, &wd), page_w, page_h);
    let (width, height) = (place.layout_w, place.layout_h);
    let (frame_c, _) = frame(doc, &wd, width, height);
    let mut content = frame_c.into_bytes();
    let caption = mk.get(b"CA").and_then(|o| doc.resolve(o).as_string().map(|s| s.to_text())).unwrap_or_default();
    let mut fonts = Dict::new();
    if !caption.is_empty() && mk.get(b"TP").and_then(|t| doc.resolve(t).as_int()) != Some(1) {
        let da = wd.get(b"DA").and_then(|o| doc.resolve(o).as_string().map(|s| s.to_text()));
        // The automatic size PdfCraft's own buttons use.
        let auto = ((height - 4.0) * 0.6).clamp(4.0, 14.0);
        let (ops, name, font, _) = button_caption(doc, da.as_deref().unwrap_or(&f.da), &caption, width, height, auto);
        content.extend(ops);
        fonts.set(name.into_bytes(), font);
    }
    let mut res = Dict::new();
    res.set(b"Font".to_vec(), Object::Dict(fonts));
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("XObject"));
    d.set(b"Subtype".to_vec(), Object::name("Form"));
    set_form_box(&mut d, place);
    d.set(b"Resources".to_vec(), Object::Dict(res));
    Stream::flate(d, &content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_appearance_strings_parse() {
        assert_eq!(parse_da("/Helv 0 Tf 0 g"), Da { font: "Helv".into(), size: 0.0, color: "0 g".into() });
        assert_eq!(parse_da("0.1 0.2 0.3 rg /F1 11 Tf"), Da { font: "F1".into(), size: 11.0, color: "0.1 0.2 0.3 rg".into() });
        assert_eq!(parse_da("/Cour 9 Tf 0 0 0 1 k").color, "0 0 0 1 k");
        assert_eq!(parse_da("garbage").font, "Helv");
        assert_eq!(stroke_op("1 0 0 rg"), "1 0 0 RG");
    }

    #[test]
    fn list_rows_visibility_and_scrolling() {
        let rows = ListRows { top: 0, inset: 2.0, row: 11.5, size: 10.0 };
        assert_eq!(rows.visible(46.0), 3);
        assert_eq!(rows.option_at(0, 1.0), None);
        assert_eq!(rows.option_at(0, 5.0), Some(0));
        assert_eq!(rows.option_at(0, 15.0), Some(1));
        assert_eq!(rows.option_at(2, 5.0), Some(2));

        // When Tue (1) and Thu (3) are selected in 5 options with 3 visible rows:
        assert_eq!(ListRows::top_showing(0, &[1, 3], 3, 5), 1);
        // When staying within view:
        assert_eq!(ListRows::top_showing(1, &[1, 2], 3, 5), 1);
        // When selecting before current top:
        assert_eq!(ListRows::top_showing(2, &[0], 3, 5), 0);
        // When empty selection:
        assert_eq!(ListRows::top_showing(0, &[], 3, 5), 0);
    }
}
