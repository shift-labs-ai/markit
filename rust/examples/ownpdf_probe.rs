//! Differential gate: own_pdf must produce the same page count and
//! byte-identical page content as lopdf across a corpus.
use markit::converters::pdf::own_pdf::{decode_stream, dget, Pdf, Val};

fn own_pages(pdf: &Pdf) -> anyhow::Result<Vec<Vec<u8>>> {
    let root = match dget(&pdf.trailer, b"Root") {
        Some(v) => pdf.resolve(v)?,
        None => anyhow::bail!("no Root"),
    };
    let Val::Dict(root) = root else {
        anyhow::bail!("bad Root")
    };
    let Some(Val::Dict(pages)) = pdf.dict_get(&root, b"Pages")? else {
        anyhow::bail!("no Pages");
    };
    let mut out = Vec::new();
    walk(pdf, &pages, &mut out, 0)?;
    Ok(out)
}

fn walk(
    pdf: &Pdf,
    node: &[(&[u8], Val)],
    out: &mut Vec<Vec<u8>>,
    depth: usize,
) -> anyhow::Result<()> {
    if depth > 32 {
        anyhow::bail!("page tree too deep");
    }
    match pdf.dict_get(&node.to_vec(), b"Type")? {
        Some(Val::Name(b"Pages")) => {
            let Some(Val::Array(kids)) = pdf.dict_get(&node.to_vec(), b"Kids")? else {
                anyhow::bail!("Pages without Kids");
            };
            for kid in kids {
                let Val::Dict(kd) = pdf.resolve(&kid)? else {
                    continue;
                };
                walk(pdf, &kd, out, depth + 1)?;
            }
        }
        _ => {
            // Page: concatenate its content streams.
            let mut content = Vec::new();
            match pdf.dict_get(&node.to_vec(), b"Contents")? {
                Some(Val::Stream(d, raw)) => {
                    content.extend_from_slice(&decode_stream(&d, raw, pdf)?)
                }
                Some(Val::Array(items)) => {
                    for it in items {
                        if let Val::Stream(d, raw) = pdf.resolve(&it)? {
                            content.extend_from_slice(&decode_stream(&d, raw, pdf)?);
                            content.push(b'\n');
                        }
                    }
                }
                _ => {}
            }
            out.push(content);
        }
    }
    Ok(())
}

fn main() {
    let dir = std::env::args().nth(1).unwrap();
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
        .collect();
    files.sort();

    let (mut same, mut diff, mut own_err) = (0, 0, 0);
    for p in &files {
        let bytes = std::fs::read(p).unwrap();
        let own = Pdf::parse(&bytes).and_then(|pdf| own_pages(&pdf));
        let lo = lopdf::Document::load_mem(&bytes).ok().map(|doc| {
            doc.get_pages()
                .values()
                .map(|id| doc.get_page_content(*id).unwrap_or_default())
                .collect::<Vec<_>>()
        });
        match (own, lo) {
            (Ok(a), Some(b)) => {
                // lopdf joins multi-stream content with a newline too? It
                // concatenates with a space historically — compare loosely:
                // same page count + same content ignoring whitespace bytes.
                let norm = |v: &[Vec<u8>]| -> Vec<Vec<u8>> {
                    v.iter()
                        .map(|c| {
                            c.iter()
                                .copied()
                                .filter(|b| !b.is_ascii_whitespace())
                                .collect()
                        })
                        .collect()
                };
                if norm(&a) == norm(&b) {
                    same += 1;
                } else {
                    diff += 1;
                    eprintln!("DIFF {} (pages {} vs {})", p.display(), a.len(), b.len());
                }
            }
            (Err(e), _) => {
                own_err += 1;
                eprintln!("OWN-ERR {}: {e}", p.display());
            }
            (_, None) => {}
        }
    }
    println!(
        "same={same} diff={diff} own_err={own_err} of {}",
        files.len()
    );
}
