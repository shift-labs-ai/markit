//! Phase timing for the HTML conversion path.
use markit::utils::html_to_md::{html_to_markdown, normalize_tables_html};
use std::time::Instant;

fn med(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: html_profile <file.html>");
    let html = std::fs::read_to_string(&path).unwrap();
    let n = 30;

    let mut t_norm = vec![];
    let mut t_full = vec![];
    let mut t_parse = vec![];
    let mut normalized = String::new();
    for _ in 0..n {
        let t = Instant::now();
        normalized = normalize_tables_html(&html);
        t_norm.push(t.elapsed().as_secs_f64() * 1000.0);

        let t = Instant::now();
        let _doc = scraper::Html::parse_document(&normalized);
        t_parse.push(t.elapsed().as_secs_f64() * 1000.0);

        let t = Instant::now();
        let md = html_to_markdown(&normalized);
        t_full.push(t.elapsed().as_secs_f64() * 1000.0);
        std::hint::black_box(md);
    }
    let (parse, full) = (med(t_parse), med(t_full));
    println!(
        "normalize_tables={:.2}ms scraper_parse={:.2}ms html_to_markdown_total={:.2}ms (walk={:.2}ms)",
        med(t_norm), parse, full, full - parse
    );
    std::hint::black_box(normalized);
}
