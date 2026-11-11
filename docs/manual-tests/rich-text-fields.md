# Manual tests: rich text form fields

Checks for filling and formatting rich text form fields in the desktop app (#3 to #22). Run them
after changes to the field editor, the Format text panel or rich field drawing, and before an
upstream pull request. Automated tests cover most of this; these catch what only shows in the
running app.

## Setup

- Build with craft-fonts at the revision CI pins (`.github/workflows/ci.yml`), so the open fonts
  and symbol faces are there:

  ```sh
  git clone https://github.com/storytold/craft-fonts ../craft-fonts
  git -C ../craft-fonts checkout <pinned revision>
  CRAFT_FONTS_DIR="$(cd ../craft-fonts && pwd)" cargo run --release -p pdfcraft -- form.pdf
  ```

  `CRAFT_FONTS_DIR` must be an absolute path: the build script runs in `crates/fonts`, so a
  relative path is read from there. A build that can't find it prints
  `building without craft-fonts` and goes on without the open fonts and symbol faces.

- Use a form with rich text fields written by another app (Acrobat), in a font the file doesn't
  embed (Calibri), with several paragraphs and some bold runs, plus a few plain text fields. Use a
  form that only holds allowed assets if screenshots are taken (AGENTS.md §1.2).
- Note the commit tested (`git log -1 --oneline`) with each run below.

## Checks

### Opening and committing

| # | Do | Expect |
|---|----|--------|
| 1 | Click into a rich field and click away without changing anything. | It looks identical; Undo isn't offered. |
| 2 | Edit a word, commit, then Undo (⌘Z / Ctrl+Z) and Redo. | Undo brings back the field's value and formatting, as PdfCraft draws it (not necessarily the other app's original drawing, #33); Redo re-applies the edit. |
| 3 | Change something, then press Escape. | Everything since opening the field is discarded. |
| 4 | Type at the start, middle and end of a bold run. | New text takes the style of the character before it (after it, at the very start). |

### Formatting

| # | Do | Expect |
|---|----|--------|
| 5 | Select part of a word; apply bold, italic and underline from the panel, then with ⌘B / ⌘I / ⌘U (Ctrl elsewhere). | Each toggles on the selection only; nothing is deleted. |
| 6 | With nothing selected, toggle bold (and underline), then type. | Only the new text is bold (underlined). |
| 7 | Change the size (a preset, then a typed value and Enter) and the colour of part of a line; then select across two sizes. | Applied in the editor and drawn after commit; a selection of two sizes shows "Mixed". |
| 8 | Open the font list; pick Carlito, Liberation Serif, then the form's own font (Calibri). | The list offers Helvetica, Times, Courier, the form's fonts and the craft-fonts families; the pick shows in the editor and is drawn after commit. |
| 9 | Centre one paragraph and leave the next; also select across two paragraphs and right-align them. | Drawn so after commit (the open editor shows text left-aligned, by design); the panel keeps the selection. |
| 10 | With the left panel closed, press ⌘E / Ctrl+E in a rich field. | The panel opens with Format text; the field stays open. |
| 11 | With no field open, press ⌘B / Ctrl+B on the page. | A bookmark is added. |

### Saving and other viewers

| # | Do | Expect |
|---|----|--------|
| 12 | Save, close and reopen in PdfCraft. | Formatting, sizes, fonts and alignment are kept. |
| 13 | Open the saved file in another viewer. | The same runs; a craft-fonts run is embedded (no font substitution). |
| 14 | Fill a field, save, fill it again, save. | The file opens cleanly each time. |

### Edge cases

| # | Do | Expect |
|---|----|--------|
| 15 | Paste a very long text, and a value with an emoji or CJK, into a rich field. | `?` and a notice for what no font has; never a crash. |
| 16 | Type in a plain (not rich) text field; press ⌘B there. | Edits as before; ⌘B does nothing; Format text doesn't appear. |
| 17 | Set a field's size to Auto and give it a long bold run. | It shrinks to fit the box. |
| 18 | Zoom and scroll while a rich field is open. | The editor follows the field. |
| 19 | Tab from a rich field. | It commits and moves to the next field. |
| 20 | Type ☐ into a field in a Calibri form. | Drawn as ☐ from the symbol face, with no "can't draw" notice. |
| 21 | Press Enter twice in a rich field, type, commit and reopen. | The blank line is drawn and kept. |
| 22 | Open Field Properties on a field in the form's own font. | It shows that font and offers it; OK without changes keeps it. |

### Automation

| # | Do | Expect |
|---|----|--------|
| 23 | `form_fields` | Rich fields have `rich_text: true`; `rich_fonts` lists the families. |
| 24 | `form_fill` with `rich` runs using `font: "carlito"`, `align: "center"` and `"\n\n"`. | Written and drawn so; the blank line is kept. |

## Results

### Run 1 (2026-11-10)

Build: a local build of `main` after #21 (the commit wasn't noted), with `CRAFT_FONTS_DIR=../craft-fonts`: a relative path, so most likely built without craft-fonts. Checks 12 to 24 weren't run.

| # | Result | Notes |
|---|--------|-------|
| 1 | Pass | |
| 2 | Fail | Undo doesn't bring back the appearance the other app drew: #33. |
| 3 | Pass | |
| 4 | Pass | |
| 5 | Fail | Ctrl+U deletes the text before underlining it (Ctrl+Z brings it back): #28. The rest passes. |
| 6 | Fail | Ctrl+U deletes the last character or the selection; typing then underlines: #28. |
| 7 | Fail | Shown in the editor, not drawn after commit; reopening the field shows it again: #30. |
| 8 | Fail | The fonts aren't in the list; a picked font isn't drawn reliably in the editor or after commit: #31, #27. |
| 9 | Fail | Alignment isn't applied in the editor or after commit: #32. Every panel control clears the selection: #29. |
| 10 | Pass | |
| 11 | Pass | |

Also reported: ☐ still shows the "can't draw" notice (#24); Field Properties can't change a field in the form's own font (#25); blank lines are lost on commit (#26).

### Run 2 (2026-11-11)

Build: `main` with #36 (fixes for #28, #29, #30 and #32), craft-fonts found through an absolute
`CRAFT_FONTS_DIR`.

| # | Result | Notes |
|---|--------|-------|
| 1 | Pass | |
| 2 | Pass | Bringing back the other app's drawing on undo is no longer required (#33, not planned). |
| 3 to 20 | Pass | |
| 21 | Fail | The blank line isn't kept (#26); spaces at the start and end of lines are trimmed (#37). |
| 22 | Pass | |
