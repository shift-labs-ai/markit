//! Which encodings do the fonts actually referenced by pages use?
use markit::converters::pdf::own_pdf::{dget, Pdf, Val};

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let data = std::fs::read(&path).unwrap();
    let pdf = Pdf::parse(&data).unwrap();
    let mut names = std::collections::BTreeMap::<String, usize>::new();
    for num in pdf.object_numbers() {
        let Ok(Val::Dict(d)) = pdf.object(num) else {
            continue;
        };
        if !matches!(dget(&d, b"Type"), Some(Val::Name(b"Page"))) {
            continue;
        }
        let Ok(Some(Val::Dict(res))) = pdf.dict_get(&d, b"Resources") else {
            continue;
        };
        let Ok(Some(Val::Dict(fonts))) = pdf.dict_get(&res, b"Font") else {
            continue;
        };
        for (_, obj) in &fonts {
            let Ok(Val::Dict(fd)) = pdf.resolve(obj) else {
                continue;
            };
            let sub = pdf
                .dict_get(&fd, b"Subtype")
                .ok()
                .flatten()
                .and_then(|v| v.as_name().map(|n| String::from_utf8_lossy(n).into_owned()))
                .unwrap_or_default();
            let enc = match pdf.dict_get(&fd, b"Encoding") {
                Ok(Some(Val::Name(n))) => String::from_utf8_lossy(n).into_owned(),
                Ok(Some(Val::Dict(_))) => "<dict>".into(),
                Ok(Some(Val::Stream(..))) => "<embedded stream>".into(),
                other => format!("{other:?}"),
            };
            *names.entry(format!("{sub} {enc}")).or_default() += 1;
        }
    }
    for (k, v) in names {
        println!("{v:5} {k}");
    }
}
