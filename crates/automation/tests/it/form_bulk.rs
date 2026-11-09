//! End-to-end bulk Field Properties through the same tools used by CLI and MCP.

use pdfcraft_automation::{Automation, Content, ToolError};
use pdfcraft_engine::{DocId, Edit, FieldFont, FieldProps, FieldValue, NewField, Session, field_flags};
use serde_json::{Value, json};

fn call(a: &mut Automation, args: Value) -> Value {
    match a.call("form_set_props", &args).unwrap().remove(0) {
        Content::Json(v) => v,
        other => panic!("expected JSON: {other:?}"),
    }
}

fn tool(a: &mut Automation, name: &str, args: Value) -> Value {
    match a.call(name, &args).unwrap().remove(0) {
        Content::Json(v) => v,
        other => panic!("expected JSON: {other:?}"),
    }
}

fn setup(test: &str) -> (Automation, u64, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("pdfcraft-form-bulk-{test}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = Session::new();
    let bytes = s.create_blank(300.0, 400.0, 2).unwrap();
    let id = s.open("form.pdf", None, bytes, None).unwrap();
    for (name, page, x) in [("alpha", 0, 30.0), ("beta", 1, 40.0)] {
        s.apply(id, Edit::AddField { page, rect: [x, 300.0, x + 180.0, 324.0], kind: NewField::Text { multiline: false }, name: Some(name.into()) })
            .unwrap();
        s.apply(id, Edit::SetFieldValue { name: name.into(), value: FieldValue::Text(name.into()) }).unwrap();
        let mut look = s.get(id).unwrap().field_look(name).unwrap();
        look.fill = Some(if name == "alpha" { [1.0, 0.9, 0.8] } else { [0.8, 0.9, 1.0] });
        look.font = if name == "alpha" { FieldFont::Courier } else { FieldFont::Times };
        s.apply(
            id,
            Edit::SetFieldProps {
                name: name.into(),
                props: Box::new(FieldProps { look: Some(look), tooltip: Some(name.into()), ..Default::default() }),
            },
        )
        .unwrap();
    }
    std::fs::write(dir.join("form.pdf"), s.save_bytes(id).unwrap().as_slice()).unwrap();
    // Saves stamp /ModDate from the clock: pin it, or two saves a second apart differ.
    let mut a = Automation::new().with_root(&dir).unwrap().with_clock(|| 1_700_000_000);
    let id = tool(&mut a, "doc_open", json!({"path": "form.pdf"}))["doc"].as_u64().unwrap();
    (a, id, dir)
}

fn bytes(a: &Automation, id: u64) -> Vec<u8> {
    a.session().save_bytes(DocId(id)).unwrap().as_ref().clone()
}

#[test]
fn bulk_properties_are_one_undo_step_preserve_other_looks_and_survive_save() {
    let (mut a, id, dir) = setup("roundtrip");
    let before = bytes(&a, id);
    let original =
        [a.session().get(DocId(id)).unwrap().field_look("alpha").unwrap(), a.session().get(DocId(id)).unwrap().field_look("beta").unwrap()];
    let out = call(&mut a, json!({"doc": id, "fields": ["alpha", "beta"], "required": true, "appearance": {"width": 4, "border": "#FF0000"}}));
    assert_eq!(out["fields"], json!(["alpha", "beta"]));
    let doc = a.session().get(DocId(id)).unwrap();
    assert_eq!(doc.can_undo(), Some("Change field properties"));
    for (name, look) in [("alpha", original[0]), ("beta", original[1])] {
        let changed = doc.field_look(name).unwrap();
        assert_eq!(changed.width, 4.0);
        assert_eq!(changed.border, Some([1.0, 0.0, 0.0]));
        assert_eq!((changed.fill, changed.font, changed.text), (look.fill, look.font, look.text));
        let f = doc.form.iter().find(|f| f.name == name).unwrap();
        assert!(f.has(field_flags::REQUIRED));
        assert_eq!(f.value, [name]);
        assert_eq!(f.tooltip.as_deref(), Some(name));
    }
    tool(&mut a, "edit_undo", json!({"doc": id}));
    assert_eq!(bytes(&a, id), before);
    tool(&mut a, "edit_redo", json!({"doc": id}));
    tool(&mut a, "doc_save", json!({"doc": id, "path": "saved.pdf"}));
    let reopened = tool(&mut a, "doc_open", json!({"path": "saved.pdf"}))["doc"].as_u64().unwrap();
    let doc = a.session().get(DocId(reopened)).unwrap();
    assert_eq!(doc.field_look("beta").unwrap().font, original[1].font);
    assert_eq!(doc.field_look("beta").unwrap().width, 4.0);
    assert_eq!(doc.form.iter().find(|f| f.name == "beta").unwrap().value, ["beta"]);
    let image = a.call("page_render", &json!({"doc": reopened, "page": 2, "dpi": 72})).unwrap();
    assert!(image.iter().any(|c| matches!(c, Content::Png { width: 300, height: 400, .. })));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn bulk_property_arguments_are_strict_and_fail_without_any_edit() {
    let (mut a, id, dir) = setup("invalid");
    let before = bytes(&a, id);
    let cases = [
        json!({"doc": id, "required": true}),
        json!({"doc": id, "field": "alpha", "fields": ["beta"], "required": true}),
        json!({"doc": id, "fields": [], "required": true}),
        json!({"doc": id, "fields": ["alpha", "alpha"], "required": true}),
        json!({"doc": id, "fields": ["alpha", 7], "required": true}),
        json!({"doc": id, "fields": ["alpha", "beta"], "name": "same"}),
        json!({"doc": id, "fields": ["alpha", "beta"], "rotation": 90}),
        json!({"doc": id, "fields": ["alpha", "beta"], "rect": [0, 0, 20, 20]}),
        json!({"doc": id, "fields": ["alpha", "beta"]}),
        json!({"doc": id, "fields": ["alpha", "beta"], "required": "yes"}),
        json!({"doc": id, "fields": ["alpha", "beta"], "appearance": {"style": "invalid"}}),
        json!({"doc": id, "fields": ["alpha", "beta"], "appearance": {"width": -1}}),
        json!({"doc": id, "fields": ["alpha", "beta"], "appearance": {"width": "thick"}}),
        json!({"doc": id, "fields": ["alpha", "beta"], "appearance": {"style": 3}}),
        json!({"doc": id, "fields": ["alpha", "beta"], "appearance": {"font": true}}),
        json!({"doc": id, "fields": ["alpha", "beta"], "appearance": {"unknown": true}}),
        json!({"doc": id, "fields": ["alpha", "beta"], "requred": true}),
        json!({"doc": id, "fields": vec!["alpha"; 1001], "required": true}),
    ];
    for args in cases {
        assert!(matches!(a.call("form_set_props", &args), Err(ToolError::InvalidArgs(_))), "{args}");
        assert_eq!(bytes(&a, id), before, "{args}");
    }
    assert!(matches!(a.call("form_set_props", &json!({"doc": id, "fields": ["alpha", "missing"], "required": true})), Err(ToolError::Failed(_))));
    assert_eq!(bytes(&a, id), before);
    assert!(a.session().get(DocId(id)).unwrap().can_undo().is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_locked_last_field_rolls_back_earlier_changes_and_unlocks_with_the_batch() {
    let (mut a, id, dir) = setup("locked");
    call(&mut a, json!({"doc": id, "field": "beta", "locked": true}));
    let before = bytes(&a, id);
    assert!(matches!(
        a.call("form_set_props", &json!({"doc": id, "fields": ["alpha", "beta"], "tooltip": "Shared help"})),
        Err(ToolError::Failed(_))
    ));
    assert_eq!(bytes(&a, id), before);
    assert_eq!(a.session().get(DocId(id)).unwrap().can_undo(), Some("Change field properties"));
    call(&mut a, json!({"doc": id, "fields": ["alpha", "beta"], "locked": false, "tooltip": "Shared help"}));
    assert!(a.session().get(DocId(id)).unwrap().form.iter().all(|f| !f.locked() && f.tooltip.as_deref() == Some("Shared help")));
    tool(&mut a, "edit_undo", json!({"doc": id}));
    assert_eq!(bytes(&a, id), before);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn bulk_options_apply_per_field_and_refuse_an_invalid_comb_atomically() {
    let (mut a, id, dir) = setup("options");
    let before = bytes(&a, id);
    assert!(matches!(a.call("form_set_props", &json!({"doc": id, "fields": ["alpha", "beta"], "flags": {"comb": true}})), Err(ToolError::Failed(_))));
    assert_eq!(bytes(&a, id), before);
    call(&mut a, json!({"doc": id, "fields": ["alpha", "beta"], "max_length": 20, "align": "center", "flags": {"comb": true, "spell_check": false}}));
    let doc = a.session().get(DocId(id)).unwrap();
    assert!(doc.form.iter().all(|f| f.max_len == Some(20) && f.quadding == 1 && f.has(field_flags::COMB) && f.has(field_flags::DO_NOT_SPELL_CHECK)));
    assert_eq!(doc.form.iter().map(|f| f.value[0].as_str()).collect::<Vec<_>>(), ["alpha", "beta"]);
    tool(&mut a, "edit_undo", json!({"doc": id}));
    assert_eq!(bytes(&a, id), before);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn button_export_edit_is_one_undo_step_and_survives_save() {
    let (mut a, id, dir) = setup("button-export");
    tool(&mut a, "form_add_field", json!({"doc": id, "page": 1, "type": "checkbox", "name": "agree", "rect": [20, 20, 40, 40]}));
    tool(&mut a, "form_fill", json!({"doc": id, "values": {"agree": true}}));
    let before = bytes(&a, id);
    call(&mut a, json!({"doc": id, "field": "agree", "export_value": "Accepted", "widget": 1}));
    let f = tool(&mut a, "form_fields", json!({"doc": id}));
    let check = f["fields"].as_array().unwrap().iter().find(|f| f["name"] == "agree").unwrap();
    assert_eq!(check["export_values"], json!(["Accepted"]));
    assert_eq!(check["value"], true);
    tool(&mut a, "edit_undo", json!({"doc": id}));
    assert_eq!(bytes(&a, id), before);
    tool(&mut a, "edit_redo", json!({"doc": id}));
    tool(&mut a, "doc_save", json!({"doc": id, "path": "saved.pdf"}));
    let reopened = tool(&mut a, "doc_open", json!({"path": "saved.pdf"}))["doc"].as_u64().unwrap();
    let f = tool(&mut a, "form_fields", json!({"doc": reopened}));
    let check = f["fields"].as_array().unwrap().iter().find(|f| f["name"] == "agree").unwrap();
    assert_eq!(check["export_values"], json!(["Accepted"]));
    assert_eq!(check["value"], true);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Issue #1: `form_set_props` takes every standard font by name, bold and italic included; the
/// choice survives a save and reopen, and an unknown name is refused with the names it takes.
#[test]
fn bold_and_italic_field_fonts_through_form_set_props() {
    let (mut a, id, dir) = setup("bold-italic");
    for font in FieldFont::ALL {
        call(&mut a, json!({"doc": id, "field": "alpha", "appearance": {"font": font.key()}}));
        assert_eq!(a.session().get(DocId(id)).unwrap().field_look("alpha").unwrap().font, font, "{}", font.key());
    }
    call(&mut a, json!({"doc": id, "field": "alpha", "appearance": {"font": "times-bold-italic"}}));
    let mut s = Session::new();
    let reopened = s.open("saved.pdf", None, std::sync::Arc::new(bytes(&a, id)), None).unwrap();
    assert_eq!(s.get(reopened).unwrap().field_look("alpha").unwrap().font, FieldFont::TimesBoldItalic);
    let err = a.call("form_set_props", &json!({"doc": id, "field": "alpha", "appearance": {"font": "comic"}})).unwrap_err();
    assert!(matches!(&err, ToolError::InvalidArgs(m) if m.contains("times-bold-italic")), "{err:?}");
    std::fs::remove_dir_all(dir).ok();
}

/// Issue #4: form_fill writes formatted text to a text field through the same tool CLI and MCP
/// use: the plain text is the value, the field becomes rich text (form_fields says so) and keeps
/// it through save and reopen; arguments it can't use are refused with what it takes instead.
#[test]
fn formatted_text_through_form_fill() {
    let (mut a, id, dir) = setup("rich");
    let runs =
        json!([{"text": "Sneak Attack", "bold": true}, {"text": "\nOnce per turn, "}, {"text": "2d6", "bold": true, "color": "red", "size": 9}]);
    let out = tool(&mut a, "form_fill", json!({"doc": id, "rich": {"alpha": runs}}));
    assert_eq!((out["filled"].as_u64(), out["undo"].as_str()), (Some(1), Some("Fill in alpha")), "{out}");
    let field = |a: &mut Automation, name: &str| {
        tool(a, "form_fields", json!({"doc": id}))["fields"].as_array().unwrap().iter().find(|f| f["name"] == name).cloned().unwrap()
    };
    let alpha = field(&mut a, "alpha");
    assert_eq!((alpha["value"].as_str(), alpha["rich_text"].as_bool()), (Some("Sneak Attack\rOnce per turn, 2d6"), Some(true)), "{alpha}");
    assert!(field(&mut a, "beta").get("rich_text").is_none(), "only the field written");
    tool(&mut a, "doc_save", json!({"doc": id, "path": "rich.pdf"}));
    let reopened = tool(&mut a, "doc_open", json!({"path": "rich.pdf"}))["doc"].as_u64().unwrap();
    let alpha =
        tool(&mut a, "form_fields", json!({"doc": reopened}))["fields"].as_array().unwrap().iter().find(|f| f["name"] == "alpha").cloned().unwrap();
    assert_eq!(alpha["rich_text"].as_bool(), Some(true), "kept through save and reopen: {alpha}");
    for (args, says) in [
        (json!({"alpha": [{"text": "x", "weight": 700}]}), "runs take text, bold"),
        (json!({"alpha": [{"text": "x", "size": 900}]}), "size must be 1 to 300"),
        (json!({"alpha": [{"text": "x", "font": "comic"}]}), "helvetica, times or courier"),
        (json!({"alpha": []}), "give 1 to"),
        (json!({"alpha": "plain"}), "array of runs"),
    ] {
        let err = a.call("form_fill", &json!({"doc": id, "rich": args})).unwrap_err();
        assert!(matches!(&err, ToolError::InvalidArgs(m) if m.contains(says)), "{args}: {err:?}");
    }
    let err = a.call("form_fill", &json!({"doc": id, "values": {"alpha": "a"}, "rich": {"alpha": [{"text": "b"}]}})).unwrap_err();
    assert!(matches!(&err, ToolError::InvalidArgs(m) if m.contains("both values and rich")), "{err:?}");
    std::fs::remove_dir_all(dir).ok();
}

/// Issue #7: `form_set_props` takes the craft-fonts faces this build has by name; the field keeps
/// the font through save and reopen. Without craft-fonts the name is refused, listing the fonts
/// this build has.
#[test]
fn craft_field_fonts_through_form_set_props() {
    let (mut a, id, dir) = setup("craft-font");
    let carlito = FieldFont::all().into_iter().find(|f| f.key() == "carlito-bold");
    let args = json!({"doc": id, "field": "alpha", "appearance": {"font": "carlito-bold"}});
    let Some(carlito) = carlito else {
        let err = a.call("form_set_props", &args).unwrap_err();
        // The list after the name asked for offers only the standard fonts.
        let offered = |m: &str| m.split_once('(').map(|(_, list)| list.to_string()).unwrap_or_default();
        assert!(
            matches!(&err, ToolError::InvalidArgs(m) if offered(m).contains("courier-bold-oblique") && !offered(m).contains("carlito")),
            "{err:?}"
        );
        std::fs::remove_dir_all(dir).ok();
        return;
    };
    call(&mut a, args);
    tool(&mut a, "doc_save", json!({"doc": id, "path": "saved.pdf"}));
    let reopened = tool(&mut a, "doc_open", json!({"path": "saved.pdf"}))["doc"].as_u64().unwrap();
    assert_eq!(a.session().get(DocId(reopened)).unwrap().field_look("alpha").unwrap().font, carlito);
    assert_eq!(a.session().get(DocId(reopened)).unwrap().field_look("beta").unwrap().font, FieldFont::Times, "only the field asked");
    std::fs::remove_dir_all(dir).ok();
}

/// Issue #14: filling a field whose form font the file doesn't embed (Calibri) draws it with its
/// open twin, and says so: a warning in form_fill's result and a line in the repair log
/// (doc_info). Without craft-fonts, no such note.
#[test]
fn a_twin_drawn_for_a_missing_form_font_is_said() {
    let dir = std::env::temp_dir().join(format!("pdfcraft-form-bulk-twin-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 600 800] >>",
        "<< /Type /Page /Parent 2 0 R /Annots [5 0 R] >>",
        "<< /Fields [5 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Calibri 6 0 R >> >> >>",
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (notes) /DA (/Calibri 10 Tf 0 g) /Rect [50 700 350 720] /P 3 0 R >>",
        "<< /Type /Font /Subtype /TrueType /BaseFont /Calibri /Encoding /WinAnsiEncoding >>",
    ];
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    std::fs::write(dir.join("calibri.pdf"), pdf).unwrap();
    let mut a = Automation::new().with_root(&dir).unwrap();
    let id = tool(&mut a, "doc_open", json!({"path": "calibri.pdf"}))["doc"].as_u64().unwrap();
    let out = tool(&mut a, "form_fill", json!({"doc": id, "values": {"notes": "Sneak Attack"}}));
    let warnings: Vec<String> =
        out["warnings"].as_array().map(|w| w.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
    let repairs: Vec<String> =
        tool(&mut a, "doc_info", json!({"doc": id}))["repairs"].as_array().unwrap().iter().filter_map(|x| x.as_str().map(str::to_string)).collect();
    let said = |lines: &[String]| lines.iter().any(|w| w.contains("\"notes\"") && w.contains("Carlito for Calibri"));
    if pdfcraft_engine::FieldFont::all().iter().any(|f| f.key() == "carlito") {
        assert!(said(&warnings), "form_fill says so: {out}");
        assert!(said(&repairs), "the repair log keeps it: {repairs:?}");
    } else {
        assert!(!said(&warnings) && !said(&repairs), "{out}");
    }
    std::fs::remove_dir_all(dir).ok();
}
