//! Survey image XObject filters across a corpus.
use markit::converters::pdf::own_pdf::{dget, Pdf, Val};
use rustc_hash::FxHashMap;

fn main() {
    let dir = std::env::args().nth(1).unwrap();
    let mut filters: FxHashMap<String, usize> = FxHashMap::default();
    let mut spaces: FxHashMap<String, usize> = FxHashMap::default();
    for e in std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()) {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "pdf") {
            continue;
        }
        let b = std::fs::read(&p).unwrap();
        let Ok(pdf) = Pdf::parse(&b) else { continue };
        for num in pdf.object_numbers() {
            let Ok(Val::Stream(d, _)) = pdf.object(num) else {
                continue;
            };
            if !matches!(dget(&d, b"Subtype"), Some(Val::Name(b"Image"))) {
                continue;
            }
            let f = match pdf.dict_get(&d, b"Filter") {
                Ok(Some(Val::Name(n))) => String::from_utf8_lossy(n).into_owned(),
                Ok(Some(Val::Array(a))) => a
                    .iter()
                    .filter_map(|v| v.as_name())
                    .map(|n| String::from_utf8_lossy(n).into_owned())
                    .collect::<Vec<_>>()
                    .join("+"),
                _ => "none".into(),
            };
            *filters.entry(f).or_default() += 1;
            let cs = match pdf.dict_get(&d, b"ColorSpace") {
                Ok(Some(Val::Name(n))) => String::from_utf8_lossy(n).into_owned(),
                Ok(Some(Val::Array(_))) => "array".into(),
                _ => "none".into(),
            };
            *spaces.entry(cs).or_default() += 1;
        }
    }
    let mut f: Vec<_> = filters.into_iter().collect();
    f.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    println!("filters: {f:?}");
    let mut s: Vec<_> = spaces.into_iter().collect();
    s.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    println!("colorspaces: {s:?}");
}
