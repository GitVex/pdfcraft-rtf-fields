//! Rich text field values (ISO 32000-2 §12.7.4.3): the XHTML body in a text field's `/RV`, read
//! into paragraphs of styled spans over the default style its `/DS` and `/DA` give.
//!
//! The subset read is the one form writers use: the elements `body`, `p`, `div`, `span`, `br`,
//! `b`/`strong`, `i`/`em` and `u`, and the CSS properties `font` (shorthand), `font-family`,
//! `font-weight`, `font-style`, `font-size`, `color`, `text-decoration` and `text-align`. Other
//! elements keep their text; other properties are ignored. Every family is drawn with the standard
//! 14 font closest to it (Helvetica, Times or Courier), as XFA forms are. Anything that isn't
//! well-formed XML is not read at all, so the caller falls back to the plain value.

use pdfcraft_fonts::Std14;

/// The longest `/RV` read (in bytes), and the most XML nodes and nesting levels: a value is form
/// text, so anything larger is not drawn as rich text (the plain value is).
const MAX_LEN: usize = 256 * 1024;
const MAX_NODES: u32 = 20_000;
const MAX_DEPTH: usize = 64;

/// A standard-14 family that a CSS `font-family` maps to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Helvetica,
    Times,
    Courier,
}

impl Family {
    /// The family a CSS `font-family` list names: the first entry recognised, else `None`.
    fn of_css(list: &str) -> Option<Family> {
        list.split(',').find_map(|name| {
            let name = name.trim().trim_matches(['"', '\'']).to_ascii_lowercase();
            if name.contains("courier") || name.contains("mono") {
                Some(Family::Courier)
            } else if name.contains("times") || name == "serif" || name.contains("georgia") || name.contains("minion") {
                Some(Family::Times)
            } else if name.contains("helvetica") || name.contains("arial") || name.contains("sans") || name.contains("myriad") {
                Some(Family::Helvetica)
            } else {
                None
            }
        })
    }
}

/// A run's look.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    pub family: Family,
    pub bold: bool,
    pub italic: bool,
    /// Points; 0 is automatic (fit to the field), as in a `/DA`.
    pub size: f64,
    pub color: [f64; 3],
    pub underline: bool,
}

impl Style {
    /// The plain style of a font named `name` (a `/BaseFont` or `/DA` resource name such as
    /// `Helvetica-BoldOblique`, `TiBo` or `Arial,Bold`), at `size` in `color`. A family it
    /// doesn't name is Helvetica.
    pub fn of_font(name: &str, size: f64, color: [f64; 3]) -> Style {
        let lower = name.to_ascii_lowercase();
        // Acrobat's resource names for the standard fonts (`TiBI`, `CoOb`, …).
        let (family, bold, italic) = match name {
            "Helv" | "HeBo" | "HeOb" | "HeBO" | "TiRo" | "TiBo" | "TiIt" | "TiBI" | "Cour" | "CoBo" | "CoOb" | "CoBO" => {
                let family = if name.starts_with("Ti") {
                    Family::Times
                } else if name.starts_with("Co") {
                    Family::Courier
                } else {
                    Family::Helvetica
                };
                (
                    family,
                    matches!(name, "HeBo" | "HeBO" | "TiBo" | "TiBI" | "CoBo" | "CoBO"),
                    matches!(name, "HeOb" | "HeBO" | "TiIt" | "TiBI" | "CoOb" | "CoBO"),
                )
            }
            _ => (
                Family::of_css(lower.replace(['-', ','], " ").split_whitespace().next().unwrap_or("")).unwrap_or(Family::Helvetica),
                lower.contains("bold") || lower.contains("black") || lower.contains("heavy"),
                lower.contains("italic") || lower.contains("oblique"),
            ),
        };
        Style { family, bold, italic, size, color, underline: false }
    }

    /// The standard-14 font this style is drawn in.
    pub fn font(&self) -> Std14 {
        match (self.family, self.bold, self.italic) {
            (Family::Helvetica, false, false) => Std14::Helvetica,
            (Family::Helvetica, true, false) => Std14::HelveticaBold,
            (Family::Helvetica, false, true) => Std14::HelveticaOblique,
            (Family::Helvetica, true, true) => Std14::HelveticaBoldOblique,
            (Family::Times, false, false) => Std14::TimesRoman,
            (Family::Times, true, false) => Std14::TimesBold,
            (Family::Times, false, true) => Std14::TimesItalic,
            (Family::Times, true, true) => Std14::TimesBoldItalic,
            (Family::Courier, false, false) => Std14::Courier,
            (Family::Courier, true, false) => Std14::CourierBold,
            (Family::Courier, false, true) => Std14::CourierOblique,
            (Family::Courier, true, true) => Std14::CourierBoldOblique,
        }
    }

    /// This style with the CSS declarations in `css` (a `style` attribute or a `/DS`) applied.
    fn with_css(mut self, css: &str) -> Style {
        for decl in css.split(';') {
            let Some((prop, value)) = decl.split_once(':') else { continue };
            let value = value.trim();
            match prop.trim().to_ascii_lowercase().as_str() {
                "font" => self = self.with_font_shorthand(value),
                "font-family" => self.family = Family::of_css(value).unwrap_or(self.family),
                "font-weight" => self.bold = weight_is_bold(value).unwrap_or(self.bold),
                "font-style" => self.italic = posture_is_italic(value).unwrap_or(self.italic),
                "font-size" => self.size = css_size(value).unwrap_or(self.size),
                "color" => self.color = css_color(value).unwrap_or(self.color),
                "text-decoration" => {
                    let v = value.to_ascii_lowercase();
                    if v.contains("underline") {
                        self.underline = true;
                    } else if v.contains("none") {
                        self.underline = false;
                    }
                }
                _ => {}
            }
        }
        self
    }

    /// The CSS `font` shorthand: `[style] [weight] size[/line-height] family-list`, the form
    /// `/DS` strings use (`font: Helvetica 12pt` is read too, family first).
    fn with_font_shorthand(mut self, value: &str) -> Style {
        let mut family = Vec::new();
        for token in value.split_whitespace() {
            if let Some(italic) = posture_is_italic(token) {
                self.italic = italic;
            } else if let Some(bold) = weight_is_bold(token).filter(|_| token != "normal") {
                self.bold = bold;
            } else if let Some(size) = css_size(token.split('/').next().unwrap_or(token)) {
                self.size = size;
            } else if token != "normal" {
                family.push(token);
            }
        }
        if let Some(f) = Family::of_css(&family.join(" ")) {
            self.family = f;
        }
        self
    }
}

fn weight_is_bold(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "bold" | "bolder" => Some(true),
        "normal" | "lighter" => Some(false),
        n => n.parse::<u32>().ok().map(|w| w >= 600),
    }
}

fn posture_is_italic(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "italic" | "oblique" => Some(true),
        "normal" => Some(false),
        _ => None,
    }
}

/// A CSS length as points (`12pt`, `12px`, `12`), within the sizes a `/DA` allows.
fn css_size(v: &str) -> Option<f64> {
    let v = v.trim().to_ascii_lowercase();
    let n = v.strip_suffix("pt").or_else(|| v.strip_suffix("px")).unwrap_or(&v).trim().parse::<f64>().ok()?;
    (n.is_finite() && n > 0.0).then(|| n.clamp(1.0, 300.0))
}

/// A CSS colour: `#rgb`, `#rrggbb`, `rgb(r, g, b)` (0–255 or percentages) or a basic name.
fn css_color(v: &str) -> Option<[f64; 3]> {
    let v = v.trim().to_ascii_lowercase();
    if let Some(hex) = v.strip_prefix('#') {
        let digits: Vec<u32> = hex.chars().map(|c| c.to_digit(16)).collect::<Option<_>>()?;
        return match digits.as_slice() {
            [r, g, b] => Some([*r as f64 / 15.0, *g as f64 / 15.0, *b as f64 / 15.0]),
            [r1, r2, g1, g2, b1, b2] => Some([(r1 * 16 + r2) as f64 / 255.0, (g1 * 16 + g2) as f64 / 255.0, (b1 * 16 + b2) as f64 / 255.0]),
            _ => None,
        };
    }
    if let Some(args) = v.strip_prefix("rgb(").and_then(|r| r.strip_suffix(')')) {
        let parts: Vec<f64> = args
            .split(',')
            .map(|p| {
                let p = p.trim();
                match p.strip_suffix('%') {
                    Some(pct) => pct.trim().parse::<f64>().ok().map(|x| x / 100.0),
                    None => p.parse::<f64>().ok().map(|x| x / 255.0),
                }
            })
            .collect::<Option<_>>()?;
        return match parts.as_slice() {
            [r, g, b] if [r, g, b].iter().all(|x| x.is_finite()) => Some([r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]),
            _ => None,
        };
    }
    match v.as_str() {
        "black" => Some([0.0; 3]),
        "white" => Some([1.0; 3]),
        "red" => Some([1.0, 0.0, 0.0]),
        "green" => Some([0.0, 0.5, 0.0]),
        "blue" => Some([0.0, 0.0, 1.0]),
        "gray" | "grey" => Some([0.5; 3]),
        _ => None,
    }
}

/// A paragraph's alignment (`text-align`), when it sets one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// A run of text in one style. A `\n` in it is a line break (`<br/>`).
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Paragraph {
    pub align: Option<Align>,
    pub spans: Vec<Span>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RichValue {
    pub paragraphs: Vec<Paragraph>,
}

impl RichValue {
    /// The text without its styles: paragraphs and line breaks as `\r`, as `/V` holds them.
    pub fn plain_text(&self) -> String {
        self.paragraphs.iter().map(|p| p.spans.iter().map(|s| s.text.replace('\n', "\r")).collect::<String>()).collect::<Vec<_>>().join("\r")
    }

    /// Whether this is the rich form of `value`: the same words in the same order, whatever the
    /// line breaks and runs of spaces between them. A `/RV` another app left behind after `/V`
    /// changed is not, and is not drawn.
    pub fn matches(&self, value: &str) -> bool {
        let words = |s: &str| s.split_whitespace().map(str::to_string).collect::<Vec<_>>();
        words(&self.plain_text()) == words(value)
    }
}

/// Read a `/RV` over the default style `base` (from the `/DA`) and the field's `/DS`, if it has
/// one. `None` when it isn't well-formed XML, is too large, or holds no text.
pub fn parse(rv: &str, ds: Option<&str>, base: Style) -> Option<RichValue> {
    if rv.len() > MAX_LEN {
        return None;
    }
    let options = roxmltree::ParsingOptions { allow_dtd: false, nodes_limit: MAX_NODES, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(rv.trim_start_matches('\u{feff}'), options).ok()?;
    let base = ds.map_or(base, |ds| base.with_css(ds));
    let mut out = Reader { paragraphs: Vec::new(), current: None };
    out.block(doc.root_element(), base, None, 0);
    out.finish();
    let rich = RichValue { paragraphs: out.paragraphs };
    rich.paragraphs.iter().flat_map(|p| &p.spans).any(|s| !s.text.is_empty()).then_some(rich)
}

struct Reader {
    paragraphs: Vec<Paragraph>,
    /// The paragraph being read, if any text has started one.
    current: Option<Paragraph>,
}

impl Reader {
    fn finish(&mut self) {
        if let Some(p) = self.current.take() {
            self.paragraphs.push(p);
        }
    }

    fn push_text(&mut self, text: &str, style: Style, align: Option<Align>) {
        let p = self.current.get_or_insert_with(|| Paragraph { align, spans: Vec::new() });
        match p.spans.last_mut() {
            Some(last) if last.style == style => last.text.push_str(text),
            _ => p.spans.push(Span { text: text.to_string(), style }),
        }
    }

    /// An element and what it holds, in `style` (the element's own `style` attribute and tag
    /// applied here) and the alignment of the paragraph it is in.
    fn block(&mut self, node: roxmltree::Node, inherited: Style, align: Option<Align>, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        let tag = node.tag_name().name().to_ascii_lowercase();
        let mut style = inherited;
        match tag.as_str() {
            "b" | "strong" => style.bold = true,
            "i" | "em" => style.italic = true,
            "u" => style.underline = true,
            _ => {}
        }
        let css = node.attribute("style").unwrap_or("");
        style = style.with_css(css);
        let is_block = matches!(tag.as_str(), "p" | "div" | "body");
        let align = if is_block { text_align(css).or(align) } else { align };
        if tag == "br" {
            self.push_text("\n", style, align);
            return;
        }
        // A paragraph starts afresh, and what follows it starts another.
        if is_block && tag != "body" {
            self.finish();
        }
        for child in node.children() {
            if child.is_element() {
                self.block(child, style, align, depth.saturating_add(1));
            } else if let Some(text) = child.is_text().then(|| child.text()).flatten() {
                // Indentation between blocks is markup, not text.
                if is_block && self.current.is_none() && text.trim().is_empty() {
                    continue;
                }
                let text: String = text.chars().map(|c| if matches!(c, '\n' | '\r' | '\t') { ' ' } else { c }).collect();
                self.push_text(&text, style, align);
            }
        }
        if is_block && tag != "body" {
            self.finish();
        }
    }
}

fn text_align(css: &str) -> Option<Align> {
    css.split(';').find_map(|decl| {
        let (prop, value) = decl.split_once(':')?;
        (prop.trim().eq_ignore_ascii_case("text-align")).then_some(())?;
        match value.trim().to_ascii_lowercase().as_str() {
            "center" => Some(Align::Center),
            "right" | "end" => Some(Align::Right),
            "left" | "start" | "justify" => Some(Align::Left),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: Style = Style { family: Family::Helvetica, bold: false, italic: false, size: 12.0, color: [0.0; 3], underline: false };

    fn spans(rich: &RichValue) -> Vec<(&str, bool, bool, f64)> {
        rich.paragraphs.iter().flat_map(|p| &p.spans).map(|s| (s.text.as_str(), s.style.bold, s.style.italic, s.style.size)).collect()
    }

    #[test]
    fn spans_take_their_tags_and_css() {
        let rv = r#"<?xml version="1.0"?><body xmlns="http://www.w3.org/1999/xhtml" xmlns:xfa="http://www.xfa.org/schema/xfa-data/1.0/"><p dir="ltr">Plain <b>bold</b> <span style="font-style:italic;font-size:18pt">big italic</span></p></body>"#;
        let rich = parse(rv, None, BASE).unwrap();
        assert_eq!(
            spans(&rich),
            [("Plain ", false, false, 12.0), ("bold", true, false, 12.0), (" ", false, false, 12.0), ("big italic", false, true, 18.0)]
        );
        assert_eq!(rich.plain_text(), "Plain bold big italic");
    }

    #[test]
    fn the_default_style_comes_from_ds_then_each_element() {
        let rv = r#"<body><p style="text-align:center"><span style="font-family:'Times New Roman';color:#ff0000;text-decoration:underline">red</span> rest</p></body>"#;
        let rich = parse(rv, Some("font: italic bold 10pt Courier,monospace; color:#0000ff"), BASE).unwrap();
        let p = &rich.paragraphs[0];
        assert_eq!(p.align, Some(Align::Center));
        let (red, rest) = (&p.spans[0].style, &p.spans[1].style);
        assert_eq!((red.family, red.color, red.underline, red.bold, red.size), (Family::Times, [1.0, 0.0, 0.0], true, true, 10.0));
        assert_eq!((rest.family, rest.color, rest.underline, rest.italic), (Family::Courier, [0.0, 0.0, 1.0], false, true));
        assert_eq!(red.font(), Std14::TimesBoldItalic);
    }

    #[test]
    fn paragraphs_and_line_breaks_become_returns() {
        let rv = "<body>\n  <p>one</p>\n  <p>two<br/>three</p>\n</body>";
        let rich = parse(rv, None, BASE).unwrap();
        assert_eq!(rich.paragraphs.len(), 2);
        assert_eq!(rich.plain_text(), "one\rtwo\rthree");
        assert!(rich.matches("one\r\ntwo three"), "line breaks and spaces don't count");
        assert!(!rich.matches("one two four"), "different words do");
    }

    #[test]
    fn malformed_empty_or_oversized_values_are_not_read() {
        for rv in ["<body><p>open", "not xml", "<body><p> </p></body>", "<body>&nbsp;</body>", "<!DOCTYPE x [<!ENTITY a \"b\">]><body>&a;</body>"] {
            assert_eq!(parse(rv, None, BASE), None, "{rv}");
        }
        let huge = format!("<body>{}</body>", "x".repeat(MAX_LEN));
        assert_eq!(parse(&huge, None, BASE), None);
        let deep = format!("{}text{}", "<span>".repeat(5000), "</span>".repeat(5000));
        assert!(parse(&deep, None, BASE).is_none_or(|r| r.plain_text().len() <= 4), "deep nesting is bounded, not followed");
    }

    #[test]
    fn css_values_are_bounded() {
        assert_eq!(css_size("1e9pt"), Some(300.0));
        assert_eq!(css_size("-4pt"), None);
        assert_eq!(css_size("NaN"), None);
        assert_eq!(css_color("rgb(300, 0, 50%)"), Some([1.0, 0.0, 0.5]));
        assert_eq!(css_color("#12"), None);
        assert_eq!(weight_is_bold("700"), Some(true));
    }
}
