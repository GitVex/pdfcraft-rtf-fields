//! The formatting of a rich text field being filled in (#11): one style per character of the
//! editor's text, kept in step with the typing, shown in the editor and written back as the
//! field's rich text value (`FieldValue::Rich`) when it is committed.
//!
//! The editor is a plain `egui::TextEdit`, as Edit text's is: its text is the field's plain text
//! with a line per paragraph, laid out here run by run ([`RichDraft::job`]). Bold, italic and
//! underline from the Format text panel apply to the selection, or to what is typed next.

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontFamily, FontId, Stroke};
use pdfcraft_engine::form_rich::{Align, Paragraph, RichValue, Span, Style};

/// What a character looks like, and the alignment of the paragraph it is in.
type Look = (Style, Option<Align>);

/// A style the Format text panel turns on or off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggle {
    Bold,
    Italic,
    Underline,
}

impl Toggle {
    fn get(self, s: &Style) -> bool {
        match self {
            Toggle::Bold => s.bold,
            Toggle::Italic => s.italic,
            Toggle::Underline => s.underline,
        }
    }

    fn set(self, s: &mut Style, on: bool) {
        match self {
            Toggle::Bold => s.bold = on,
            Toggle::Italic => s.italic = on,
            Toggle::Underline => s.underline = on,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RichDraft {
    /// One look per character of [`Self::text`].
    looks: Vec<Look>,
    /// The text the looks belong to: the editor's, as last synced.
    text: String,
    /// The look of text typed where there is nothing to take one from (the field's `/DA`).
    base: Style,
    /// The look of the next text typed, set by a style turned on or off with nothing selected.
    next: Option<Look>,
    /// The text and looks the editor opened with, to tell whether anything changed.
    opened: (String, Vec<Look>),
}

impl RichDraft {
    /// A draft of `rich` (the field's rich value), whose text in the editor is
    /// [`Self::editor_text`]; with no rich value, `text` in the field's own style `base`.
    pub fn open(rich: Option<&RichValue>, base: Style, text: &str) -> RichDraft {
        let (text, looks) = match rich {
            Some(rich) => {
                let mut text = String::new();
                let mut looks = Vec::new();
                for (i, p) in rich.paragraphs.iter().enumerate() {
                    if i > 0 {
                        // The break takes the look of the paragraph it ends.
                        looks.push(looks.last().copied().unwrap_or((base, p.align)));
                        text.push('\n');
                    }
                    for span in &p.spans {
                        for c in span.text.chars() {
                            text.push(if matches!(c, '\r') { '\n' } else { c });
                            looks.push((span.style, p.align));
                        }
                    }
                }
                (text, looks)
            }
            None => {
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                let looks = vec![(base, None); text.chars().count()];
                (text, looks)
            }
        };
        RichDraft { opened: (text.clone(), looks.clone()), looks, text, base, next: None }
    }

    /// The editor's text: the field's plain text, a line per paragraph.
    pub fn editor_text(&self) -> &str {
        &self.text
    }

    /// Follow the editor's text to `text`: the characters typed take the look set for the next
    /// text, else the look of the character before them (after them at the start), else the
    /// field's own; deleted characters take theirs with them.
    pub fn sync(&mut self, text: &str) {
        if text == self.text {
            return;
        }
        let old: Vec<char> = self.text.chars().collect();
        let new: Vec<char> = text.chars().collect();
        let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let room = old.len().min(new.len()).saturating_sub(prefix);
        let suffix = old.iter().rev().zip(new.iter().rev()).take(room).take_while(|(a, b)| a == b).count();
        let removed = prefix..old.len().saturating_sub(suffix);
        let inserted = new.len().saturating_sub(suffix).saturating_sub(prefix);
        let look = self
            .next
            .take()
            .or_else(|| prefix.checked_sub(1).and_then(|i| self.looks.get(i).copied()))
            .or_else(|| self.looks.get(removed.end).copied())
            .unwrap_or((self.base, None));
        if removed.end <= self.looks.len() && removed.start <= removed.end {
            self.looks.splice(removed, std::iter::repeat_n(look, inserted));
        } else {
            self.looks = vec![look; new.len()];
        }
        self.text = text.to_string();
    }

    /// Whether every character of `range` (characters) has `what` on; the look at the caret (the
    /// next text's, else the character before it) for an empty range.
    pub fn is_on(&self, range: &std::ops::Range<usize>, what: Toggle) -> bool {
        if range.is_empty() {
            return what.get(&self.caret_look(range.start).0);
        }
        self.looks.get(range.clone()).is_some_and(|l| !l.is_empty() && l.iter().all(|(s, _)| what.get(s)))
    }

    fn caret_look(&self, at: usize) -> Look {
        self.next
            .or_else(|| at.checked_sub(1).and_then(|i| self.looks.get(i).copied()))
            .or_else(|| self.looks.first().copied())
            .unwrap_or((self.base, None))
    }

    /// Turn `what` on for the characters of `range`, or off when all of them have it; with an
    /// empty range, for the text typed next at its place.
    pub fn toggle(&mut self, range: &std::ops::Range<usize>, what: Toggle) {
        let on = !self.is_on(range, what);
        if range.is_empty() {
            let mut look = self.caret_look(range.start);
            what.set(&mut look.0, on);
            self.next = Some(look);
            return;
        }
        let end = range.end.min(self.looks.len());
        for (s, _) in self.looks.get_mut(range.start.min(end)..end).into_iter().flatten() {
            what.set(s, on);
        }
    }

    /// Whether the text or its formatting differs from what the editor opened with.
    pub fn changed(&self) -> bool {
        (self.text.as_str(), &self.looks) != (self.opened.0.as_str(), &self.opened.1)
    }

    /// The draft as the field's rich text value: a paragraph per line, a span per run of one
    /// style, each paragraph with the alignment of its first character.
    pub fn value(&self) -> RichValue {
        let mut paragraphs = vec![Paragraph { align: None, spans: Vec::new() }];
        for (c, (style, align)) in self.text.chars().zip(&self.looks) {
            if c == '\n' {
                paragraphs.push(Paragraph { align: None, spans: Vec::new() });
                continue;
            }
            let Some(p) = paragraphs.last_mut() else { continue };
            if p.spans.is_empty() {
                p.align = *align;
            }
            match p.spans.last_mut() {
                Some(last) if last.style == *style => last.text.push(c),
                _ => p.spans.push(Span { text: c.to_string(), style: *style }),
            }
        }
        RichValue { paragraphs }
    }

    /// The editor's layout of `text` (the editor's, perhaps typed into since the last sync): each
    /// run in its weight, posture, colour and underline, at its size times `scale` (screen pixels
    /// per point), or `auto` pixels for a run sized automatically; wrapped at `wrap` when given.
    pub fn job(&self, text: &str, scale: f32, auto: f32, wrap: Option<f32>) -> LayoutJob {
        let mut draft = self.clone();
        draft.sync(text);
        let mut job = LayoutJob::default();
        job.wrap.max_width = wrap.unwrap_or(f32::INFINITY);
        job.keep_trailing_whitespace = true;
        let mut start = 0;
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        while start < chars.len() {
            let look = draft.looks.get(start).map_or(draft.base, |l| l.0);
            let mut end = start + 1;
            while end < chars.len() && draft.looks.get(end).map_or(draft.base, |l| l.0) == look {
                end += 1;
            }
            let from = chars.get(start).map_or(text.len(), |c| c.0);
            let to = chars.get(end).map_or(text.len(), |c| c.0);
            let size = if look.size > 0.0 { look.size as f32 * scale } else { auto };
            let family = if look.bold { FontFamily::Name("semibold".into()) } else { FontFamily::Proportional };
            let [r, g, b] = look.color.map(|x| (x.clamp(0.0, 1.0) * 255.0).round() as u8);
            let color = Color32::from_rgb(r, g, b);
            let format = TextFormat {
                font_id: FontId::new(size.max(1.0), family),
                color,
                italics: look.italic,
                underline: if look.underline { Stroke::new((size / 14.0).max(1.0), color) } else { Stroke::NONE },
                ..Default::default()
            };
            job.append(text.get(from..to).unwrap_or_default(), 0.0, format);
            start = end;
        }
        job
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfcraft_engine::form_rich::Family;

    const BASE: Style = Style { family: Family::Helvetica, face: None, bold: false, italic: false, size: 10.0, color: [0.0; 3], underline: false };

    fn bold() -> Style {
        Style { bold: true, ..BASE }
    }

    fn rich(parts: &[(&str, Style)]) -> RichValue {
        RichValue { paragraphs: vec![Paragraph { align: None, spans: parts.iter().map(|(t, s)| Span { text: (*t).into(), style: *s }).collect() }] }
    }

    #[test]
    fn opening_and_committing_unchanged_gives_the_same_value() {
        let value = rich(&[("Sneak ", bold()), ("Attack", BASE)]);
        let d = RichDraft::open(Some(&value), BASE, "Sneak Attack");
        assert_eq!(d.editor_text(), "Sneak Attack");
        assert!(!d.changed());
        assert_eq!(d.value(), value);
    }

    #[test]
    fn typing_takes_the_look_before_it_or_the_one_set_for_it() {
        let mut d = RichDraft::open(Some(&rich(&[("ab", bold()), ("cd", BASE)])), BASE, "abcd");
        // Typed at the boundary: the look before it (bold).
        d.sync("abXcd");
        assert_eq!(d.value(), rich(&[("abX", bold()), ("cd", BASE)]));
        // Bold turned off with the caret after X: what's typed next is plain.
        d.toggle(&(3..3), Toggle::Bold);
        d.sync("abXYcd");
        assert_eq!(d.value(), rich(&[("abX", bold()), ("Ycd", BASE)]));
        // At the very start: the look after it.
        d.sync("ZabXYcd");
        assert_eq!(d.value().paragraphs[0].spans[0], Span { text: "ZabX".into(), style: bold() });
        // Deleting takes the characters' looks with them.
        d.sync("ZYcd");
        assert_eq!(d.value(), rich(&[("Z", bold()), ("Ycd", BASE)]));
        assert!(d.changed());
    }

    #[test]
    fn toggling_a_selection_turns_a_style_on_or_off_for_all_of_it() {
        let mut d = RichDraft::open(Some(&rich(&[("ab", bold()), ("cd", BASE)])), BASE, "abcd");
        assert!(!d.is_on(&(0..4), Toggle::Bold), "mixed is not on");
        d.toggle(&(0..4), Toggle::Bold);
        assert_eq!(d.value(), rich(&[("abcd", bold())]));
        d.toggle(&(1..3), Toggle::Bold);
        assert_eq!(d.value(), rich(&[("a", bold()), ("bc", BASE), ("d", bold())]));
        d.toggle(&(0..4), Toggle::Italic);
        d.toggle(&(0..4), Toggle::Underline);
        assert!(d.is_on(&(0..4), Toggle::Italic) && d.is_on(&(0..4), Toggle::Underline));
    }

    #[test]
    fn a_plain_draft_starts_in_the_field_style_and_lines_are_paragraphs() {
        let mut d = RichDraft::open(None, BASE, "one\rtwo");
        assert_eq!(d.editor_text(), "one\ntwo");
        d.toggle(&(4..7), Toggle::Bold);
        let v = d.value();
        assert_eq!(v.paragraphs.len(), 2);
        assert_eq!(v.paragraphs[1].spans, [Span { text: "two".into(), style: bold() }]);
        assert_eq!(v.plain_text(), "one\rtwo");
    }

    #[test]
    fn the_layout_shows_each_run_in_its_look() {
        let d = RichDraft::open(Some(&rich(&[("ab", bold()), ("cd", Style { italic: true, underline: true, ..BASE })])), BASE, "abcd");
        let job = d.job("abcd", 2.0, 20.0, None);
        assert_eq!(job.sections.len(), 2);
        assert_eq!(job.sections[0].format.font_id.family, FontFamily::Name("semibold".into()));
        assert_eq!(job.sections[0].format.font_id.size, 20.0, "10 pt at 2 px per point");
        assert!(job.sections[1].format.italics && job.sections[1].format.underline != Stroke::NONE);
        // Text typed since the last sync is laid out with the looks it will take.
        let range = &d.job("abXcd", 2.0, 20.0, None).sections[0].byte_range;
        assert_eq!((range.start.0, range.end.0), (0, 3));
    }
}
