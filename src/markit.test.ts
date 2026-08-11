import { describe, expect, it } from "bun:test";
import { Markit } from "./markit.js";

const PDF_WITHOUT_XREF = Buffer.from(`%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
5 0 obj << /Length 42 >> stream
BT /F1 12 Tf 20 100 Td (stdin pdf) Tj ET
endstream endobj
trailer << /Root 1 0 R >>`);

describe("Markit content sniffing", () => {
  it("detects PDF bytes when stream metadata is absent", async () => {
    await expect(new Markit().convert(PDF_WITHOUT_XREF, {})).rejects.toThrow(
      "PDF conversion requires a supported Markit native binary",
    );
  });
});
