//! Survey: fonts lacking unicode mapping but carrying font programs.
use markit::converters::pdf::own_pdf::{dget, Pdf, Val};

fn main() {
    let dir = std::env::args().nth(1).unwrap();
    let (mut total, mut no_map, mut ff1, mut ff2, mut ff3) = (0u32, 0u32, 0u32, 0u32, 0u32);
    for e in std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()) {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "pdf") {
            continue;
        }
        let data = std::fs::read(&p).unwrap();
        let Ok(pdf) = Pdf::parse(&data) else { continue };
        let mut fonts = Vec::new();
        for num in pdf.object_numbers() {
            if let Ok(Val::Dict(d)) = pdf.object(num) {
                if matches!(dget(&d, b"Type"), Some(Val::Name(b"Font"))) {
                    fonts.push(d);
                }
            }
        }
        for f in &fonts {
            total += 1;
            let g = |k: &[u8]| pdf.dict_get(f, k).ok().flatten();
            let has_tu = g(b"ToUnicode").is_some();
            let has_enc = match g(b"Encoding") {
                Some(Val::Name(_)) => true,
                Some(Val::Dict(d)) => dget(&d, b"Differences").is_some(),
                _ => false,
            };
            if has_tu || has_enc {
                continue;
            }
            no_map += 1;
            if let Some(Val::Name(st)) = g(b"Subtype") {
                println!(
                    "  {} {}",
                    p.file_name().unwrap().to_string_lossy(),
                    String::from_utf8_lossy(st)
                );
            }
            if let Some(Val::Dict(fd)) = g(b"FontDescriptor") {
                let dg = |k: &[u8]| pdf.dict_get(&fd, k).ok().flatten();
                if dg(b"FontFile").is_some() {
                    ff1 += 1;
                } else if dg(b"FontFile2").is_some() {
                    ff2 += 1;
                } else if dg(b"FontFile3").is_some() {
                    ff3 += 1;
                }
            }
        }
    }
    println!("fonts={total} unmapped={no_map} (FontFile={ff1} FontFile2={ff2} FontFile3={ff3})");
}
