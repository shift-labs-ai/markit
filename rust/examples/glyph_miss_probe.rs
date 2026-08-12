//! Which /Differences glyph names fail to map, corpus-wide?
use markit::converters::pdf::own_pdf::{dget, Pdf, Val};
use std::collections::HashMap;

fn main() {
    let dir = std::env::args().nth(1).unwrap();
    let mut misses: HashMap<String, usize> = HashMap::new();
    let mut total_names = 0usize;
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "pdf") {
            continue;
        }
        let Ok(data) = std::fs::read(&path) else {
            continue;
        };
        let Ok(pdf) = Pdf::parse(&data) else {
            continue;
        };
        for num in pdf.object_numbers() {
            let Ok(Val::Dict(d)) = pdf.object(num) else {
                continue;
            };
            if !matches!(dget(&d, b"Type"), Some(Val::Name(b"Encoding")))
                && dget(&d, b"Differences").is_none()
            {
                continue;
            }
            let Ok(Some(Val::Array(diffs))) = pdf.dict_get(&d, b"Differences") else {
                continue;
            };
            for v in &diffs {
                if let Val::Name(n) = v {
                    total_names += 1;
                    if markit::converters::pdf::glyph_name_to_unicode(n).is_none() {
                        *misses
                            .entry(String::from_utf8_lossy(n).into_owned())
                            .or_default() += 1;
                    }
                }
            }
        }
    }
    let mut sorted: Vec<_> = misses.into_iter().collect();
    sorted.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    println!(
        "total names: {total_names}, unique misses: {}",
        sorted.len()
    );
    for (name, count) in sorted.iter().take(500) {
        println!("{count:6} {name}");
    }
}
