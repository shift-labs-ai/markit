//! Page-tree traversal: Root → Pages → Kids with inheritable
//! attributes (MediaBox, Rotate, Resources), plus document-level
//! optional-content visibility.

use anyhow::{bail, Result};
use rustc_hash::FxHashSet;

use super::own_pdf::{dget, Dict, Pdf, Val};

/// Object numbers of optional-content groups switched OFF in the
/// default viewer configuration (Catalog /OCProperties /D /OFF).
pub(crate) fn collect_hidden_ocgs(pdf: &Pdf, root: &Dict) -> std::rc::Rc<FxHashSet<u32>> {
    let mut set = FxHashSet::default();
    if let Ok(Some(Val::Dict(ocp))) = pdf.dict_get(root, b"OCProperties") {
        if let Ok(Some(Val::Dict(d))) = pdf.dict_get(&ocp, b"D") {
            if let Some(Val::Array(off)) = dget(&d, b"OFF") {
                for v in off {
                    if let Val::Ref(n) = v {
                        set.insert(*n);
                    }
                }
            }
        }
    }
    std::rc::Rc::new(set)
}

/// Inheritable page-tree attributes.
#[derive(Clone, Default)]
pub(crate) struct Inherit<'a> {
    pub(crate) media: Option<Vec<f64>>,
    pub(crate) rotate: Option<f64>,
    pub(crate) resources: Option<Dict<'a>>,
}

pub(crate) fn walk_pages<'a>(
    pdf: &'a Pdf<'a>,
    node: &Dict<'a>,
    inh: &Inherit<'a>,
    out: &mut Vec<(Dict<'a>, Inherit<'a>)>,
    depth: usize,
) -> Result<()> {
    walk_pages_inner(pdf, node, inh, out, depth, &mut FxHashSet::default())
}

fn walk_pages_inner<'a>(
    pdf: &'a Pdf<'a>,
    node: &Dict<'a>,
    inh: &Inherit<'a>,
    out: &mut Vec<(Dict<'a>, Inherit<'a>)>,
    depth: usize,
    active_refs: &mut FxHashSet<u32>,
) -> Result<()> {
    if depth > 32 {
        bail!("page tree too deep");
    }
    let mut inh = inh.clone();
    if let Some(Val::Array(a)) = pdf.dict_get(node, b"MediaBox")? {
        let values: Vec<f64> = a
            .iter()
            .filter_map(|value| pdf.resolve(value).ok().and_then(|v| v.as_num()))
            .collect();
        if values.len() == 4 {
            inh.media = Some(values);
        }
    }
    if let Some(value) = pdf.dict_get(node, b"Rotate")?.and_then(|v| v.as_num()) {
        inh.rotate = Some(value);
    }
    if let Some(Val::Dict(resources)) = pdf.dict_get(node, b"Resources")? {
        inh.resources = Some(resources);
    }

    if matches!(pdf.dict_get(node, b"Type")?, Some(Val::Name(b"Pages"))) {
        let Some(Val::Array(kids)) = pdf.dict_get(node, b"Kids")? else {
            bail!("Pages without Kids");
        };
        for kid in kids {
            let object_number = match kid {
                Val::Ref(number) => Some(number),
                _ => None,
            };
            if let Some(number) = object_number {
                if !active_refs.insert(number) {
                    bail!("page tree cycle at object {number}");
                }
            }
            let result = (|| {
                let Val::Dict(dict) = pdf.resolve(&kid)? else {
                    return Ok(());
                };
                walk_pages_inner(pdf, &dict, &inh, out, depth + 1, active_refs)
            })();
            if let Some(number) = object_number {
                active_refs.remove(&number);
            }
            result?;
        }
    } else {
        out.push((node.clone(), inh));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_page_tree_cycles_by_object_identity() {
        let input = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 0 >> endobj
3 0 obj << /Type /Pages /Kids [2 0 R] /Count 0 >> endobj
trailer << /Root 1 0 R >>";
        let pdf = Pdf::parse(input).unwrap();
        let Val::Dict(root) = pdf.resolve(&Val::Ref(2)).unwrap() else {
            panic!("root must resolve");
        };
        let error = walk_pages(&pdf, &root, &Inherit::default(), &mut Vec::new(), 0).unwrap_err();
        assert!(error.to_string().contains("cycle"), "{error}");
    }

    #[test]
    fn inherits_media_rotation_and_resources_and_allows_leaf_override() {
        let input = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /MediaBox [0 0 600 800] /Rotate 90 /Resources << /ProcSet [/PDF] >> /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /Rotate 180 >> endobj
trailer << /Root 1 0 R >>";
        let pdf = Pdf::parse(input).unwrap();
        let Val::Dict(root) = pdf.resolve(&Val::Ref(2)).unwrap() else {
            panic!()
        };
        let mut pages = Vec::new();
        walk_pages(&pdf, &root, &Inherit::default(), &mut pages, 0).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(
            pages[0].1.media.as_deref(),
            Some([0.0, 0.0, 600.0, 800.0].as_slice())
        );
        assert_eq!(pages[0].1.rotate, Some(180.0));
        assert!(pages[0].1.resources.is_some());
    }

    #[test]
    fn collects_only_ocgs_in_default_off_array() {
        let input = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R /OCProperties << /D << /OFF [7 0 R 9 0 R] >> >> >> endobj
2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj
7 0 obj << /Type /OCG >> endobj
9 0 obj << /Type /OCG >> endobj
trailer << /Root 1 0 R >>";
        let pdf = Pdf::parse(input).unwrap();
        let Val::Dict(root) = pdf.resolve(&Val::Ref(1)).unwrap() else {
            panic!()
        };
        let hidden = collect_hidden_ocgs(&pdf, &root);
        assert_eq!(hidden.len(), 2);
        assert!(hidden.contains(&7) && hidden.contains(&9));
    }
}
