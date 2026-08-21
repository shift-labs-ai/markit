import { describe, expect, test } from "bun:test";
import {
  CsvConverter,
  isNative,
  Markit,
  PdfConverter,
  ZipConverter,
} from "./index.js";

describe("native Rust SDK", () => {
  test("is the only engine", () => {
    expect(isNative).toBe(true);
  });

  test("converts through the registry", async () => {
    const result = await new Markit().convert(Buffer.from("hello\n"), {
      extension: ".txt",
    });
    expect(result.markdown).toBe("hello\n");
  });

  test("named converter wrappers delegate to Rust", async () => {
    const csv = new CsvConverter();
    expect(csv.accepts({ extension: ".csv" })).toBe(true);
    const result = await csv.convert(Buffer.from("name,value\na,1\n"), {
      extension: ".csv",
    });
    expect(result.markdown).toContain("| name | value |");
    expect(new PdfConverter().accepts({ mimetype: "application/pdf" })).toBe(
      true,
    );
    expect(new ZipConverter().accepts({ extension: ".zip" })).toBe(true);
  });

  test("sniffs PDFs without stream metadata", async () => {
    const pdf = Buffer.from(`%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
5 0 obj << /Length 42 >> stream
BT /F1 12 Tf 20 100 Td (native sdk) Tj ET
endstream endobj
trailer << /Root 1 0 R >>`);
    const result = await new Markit().convert(pdf);
    expect(result.markdown).toContain("native sdk");
  });

  test("page markers are opt-in and number every physical page", async () => {
    const pdf = Buffer.from(`%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
5 0 obj << /Length 42 >> stream
BT /F1 12 Tf 20 100 Td (first page) Tj ET
endstream endobj
6 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 4 0 R >> >> /Contents 7 0 R >> endobj
7 0 obj << /Length 43 >> stream
BT /F1 12 Tf 20 100 Td (second page) Tj ET
endstream endobj
trailer << /Root 1 0 R >>`);

    const plain = await new Markit().convert(pdf, { extension: ".pdf" });
    expect(plain.markdown).not.toContain("markit:page");

    const marked = await new Markit().convert(pdf, {
      extension: ".pdf",
      pageMarkers: true,
    });
    expect(marked.markdown).toContain("<!-- markit:page 1 -->");
    expect(marked.markdown).toContain("<!-- markit:page 2 -->");
    expect(marked.markdown).toContain("second page");
  });
});
