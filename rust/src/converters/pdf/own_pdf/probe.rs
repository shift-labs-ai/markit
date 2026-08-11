//! Developer-facing encryption dictionary inspection. Kept out of
//! the lexer and production decode path.

use anyhow::{bail, Result};

use super::document::Pdf;
use super::values::{dget, Val};

/// Debug helper: describe a document's /Encrypt dictionary.
pub fn probe_encrypt_dict(data: &[u8]) -> Result<String> {
    let pdf = Pdf::parse_allow_encrypted(data)?;
    let Some(enc) = dget(&pdf.trailer, b"Encrypt") else {
        return Ok("not encrypted".into());
    };
    let Val::Dict(enc) = pdf.resolve(enc)? else {
        bail!("bad Encrypt");
    };
    let mut out = String::new();
    for (k, v) in &enc {
        let vs = match pdf.resolve(v)? {
            Val::Name(n) => String::from_utf8_lossy(n).into_owned(),
            Val::Num(n) => n.to_string(),
            Val::Str(s) => format!("<{} bytes>", s.len()),
            Val::Dict(d) => {
                let keys: Vec<String> = d
                    .iter()
                    .map(|(k, v)| {
                        format!(
                            "{}:{:?}",
                            String::from_utf8_lossy(k),
                            match v {
                                Val::Dict(inner) => inner
                                    .iter()
                                    .map(|(ik, iv)| format!(
                                        "{}={}",
                                        String::from_utf8_lossy(ik),
                                        match iv {
                                            Val::Name(n) => String::from_utf8_lossy(n).into_owned(),
                                            Val::Num(n) => n.to_string(),
                                            _ => "?".into(),
                                        }
                                    ))
                                    .collect::<Vec<_>>()
                                    .join(","),
                                _ => "?".to_string(),
                            }
                        )
                    })
                    .collect();
                keys.join(" ")
            }
            other => format!("{other:?}"),
        };
        out.push_str(&format!("{} = {vs}\n", String::from_utf8_lossy(k)));
    }
    Ok(out)
}
