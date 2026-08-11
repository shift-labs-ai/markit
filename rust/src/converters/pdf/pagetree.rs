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
    if depth > 32 {
        bail!("page tree too deep");
    }
    let mut inh = inh.clone();
    if let Some(Val::Array(a)) = pdf.dict_get(node, b"MediaBox")? {
        let v: Vec<f64> = a
            .iter()
            .filter_map(|o| pdf.resolve(o).ok().and_then(|v| v.as_num()))
            .collect();
        if v.len() == 4 {
            inh.media = Some(v);
        }
    }
    if let Some(v) = pdf.dict_get(node, b"Rotate")?.and_then(|v| v.as_num()) {
        inh.rotate = Some(v);
    }
    if let Some(Val::Dict(r)) = pdf.dict_get(node, b"Resources")? {
        inh.resources = Some(r);
    }

    if matches!(pdf.dict_get(node, b"Type")?, Some(Val::Name(b"Pages"))) {
        let Some(Val::Array(kids)) = pdf.dict_get(node, b"Kids")? else {
            bail!("Pages without Kids");
        };
        for kid in kids {
            let Val::Dict(kd) = pdf.resolve(&kid)? else {
                continue;
            };
            walk_pages(pdf, &kd, &inh, out, depth + 1)?;
        }
    } else {
        out.push((node.clone(), inh));
    }
    Ok(())
}
