//! Dump the /Encrypt dictionary of a PDF (raw scan; the trailer parse
//! works even when own_pdf bails on the encryption itself).
use markit::converters::pdf::own_pdf::probe_encrypt_dict;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let b = std::fs::read(&path).unwrap();
    match probe_encrypt_dict(&b) {
        Ok(s) => println!("{s}"),
        Err(e) => println!("error: {e}"),
    }
}
