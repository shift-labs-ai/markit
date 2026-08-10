use markit::converters::pdf::own_pdf::{dget, Pdf, Val};
fn main() {
    let pdf_bytes = std::fs::read("/tmp/ocg-test.pdf").unwrap();
    let pdf = Pdf::parse(&pdf_bytes).unwrap();
    let Ok(Some(Val::Dict(root))) = pdf.dict_get(&pdf.trailer, b"Root") else {
        panic!()
    };
    match pdf.dict_get(&root, b"OCProperties") {
        Ok(Some(Val::Dict(ocp))) => match pdf.dict_get(&ocp, b"D") {
            Ok(Some(Val::Dict(d))) => {
                println!("D.OFF = {:?}", dget(&d, b"OFF").map(|v| format!("{v:?}")))
            }
            other => println!("D = {other:?}"),
        },
        other => println!("OCProperties = {other:?}"),
    }
}
