//! Dump font + first text ops of a PDF page (fast-path debugging).
use lopdf::content::Content;
use lopdf::{Document, Object};

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let b = std::fs::read(&path).unwrap();
    let mut doc = Document::load_mem(&b).unwrap();
    if doc.is_encrypted() {
        let _ = doc.decrypt("");
    }
    for (no, id) in doc.get_pages() {
        if no > 1 {
            break;
        }
        let fonts = doc.get_page_fonts(id).unwrap_or_default();
        for (name, fd) in &fonts {
            println!(
                "font {:?}: subtype={:?} basefont={:?} enc={:?} widths={} tounicode={}",
                String::from_utf8_lossy(name),
                fd.get(b"Subtype").ok().map(|o| format!("{o:?}")),
                fd.get(b"BaseFont").ok().map(|o| format!("{o:?}")),
                fd.get(b"Encoding")
                    .ok()
                    .map(|o| format!("{o:?}"))
                    .map(|s| s.chars().take(60).collect::<String>()),
                fd.get(b"Widths").is_ok(),
                fd.get(b"ToUnicode").is_ok()
            );
        }
        let data = doc.get_page_content(id).unwrap();
        let content = Content::decode(&data).unwrap();
        let mut shown = 0;
        for op in &content.operations {
            if ["Tj", "TJ", "Tf", "BT", "Tm", "Td"].contains(&op.operator.as_str()) && shown < 14 {
                let os: Vec<String> = op
                    .operands
                    .iter()
                    .map(|o| match o {
                        Object::String(s, _) => {
                            format!("({})", String::from_utf8_lossy(&s[..s.len().min(30)]))
                        }
                        other => format!("{other:?}").chars().take(40).collect(),
                    })
                    .collect();
                println!("  {} {}", op.operator, os.join(" "));
                shown += 1;
            }
        }
    }
}
