import { describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import JSZip from "jszip";
import { DocxConverter } from "./docx.js";

const converter = new DocxConverter();

// 1x1 red PNG
const TINY_PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg==",
  "base64",
);

/**
 * Build a minimal DOCX with text and optionally an inline image.
 */
async function buildDocx(opts?: { image?: boolean }): Promise<Buffer> {
  const zip = new JSZip();
  const hasImage = opts?.image ?? false;

  // Content types
  zip.file(
    "[Content_Types].xml",
    `<?xml version="1.0" encoding="UTF-8"?>
    <Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
      <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
      <Default Extension="xml" ContentType="application/xml"/>
      <Default Extension="png" ContentType="image/png"/>
      <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
    </Types>`,
  );

  // Root rels
  zip.file(
    "_rels/.rels",
    `<?xml version="1.0" encoding="UTF-8"?>
    <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
      <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
                    Target="word/document.xml"/>
    </Relationships>`,
  );

  // Document with optional image
  const imageRun = hasImage
    ? `<w:r>
        <w:drawing>
          <wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing">
            <wp:docPr id="1" name="test image"/>
            <a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
              <a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture">
                <pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture">
                  <pic:blipFill>
                    <a:blip r:embed="rId2"/>
                  </pic:blipFill>
                </pic:pic>
              </a:graphicData>
            </a:graphic>
          </wp:inline>
        </w:drawing>
      </w:r>`
    : "";

  zip.file(
    "word/document.xml",
    `<?xml version="1.0" encoding="UTF-8"?>
    <w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
      <w:body>
        <w:p>
          <w:r><w:t>Hello from DOCX</w:t></w:r>
          ${imageRun}
        </w:p>
      </w:body>
    </w:document>`,
  );

  // Document rels
  const imageRel = hasImage
    ? `<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image"
                    Target="media/image1.png"/>`
    : "";
  zip.file(
    "word/_rels/document.xml.rels",
    `<?xml version="1.0" encoding="UTF-8"?>
    <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
      ${imageRel}
    </Relationships>`,
  );

  if (hasImage) {
    zip.file("word/media/image1.png", TINY_PNG);
  }

  const buf = await zip.generateAsync({ type: "nodebuffer" });
  return Buffer.from(buf);
}

describe("DocxConverter", () => {
  test("extracts text", async () => {
    const buffer = await buildDocx();
    const result = await converter.convert(buffer, { extension: ".docx" });
    expect(result.markdown).toContain("Hello from DOCX");
  });

  test("emits image placeholder comment without imageDir", async () => {
    const buffer = await buildDocx({ image: true });
    const result = await converter.convert(buffer, { extension: ".docx" });
    expect(result.markdown).toContain("<!-- image:");
  });

  test("saves image to disk with imageDir", async () => {
    const dir = mkdtempSync(join(tmpdir(), "docx-test-"));
    try {
      const buffer = await buildDocx({ image: true });
      const result = await converter.convert(buffer, {
        extension: ".docx",
        imageDir: dir,
      });
      expect(result.markdown).toContain("![");
      expect(result.markdown).toContain(dir);

      // Verify file was written
      const files = Bun.file(join(dir, "image_1.png"));
      expect(await files.exists()).toBe(true);
    } finally {
      rmSync(dir, { recursive: true });
    }
  });

  test("table cells with multiple paragraphs stay on one line", async () => {
    const zip = new JSZip();
    zip.file(
      "[Content_Types].xml",
      `<?xml version="1.0" encoding="UTF-8"?>
      <Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
        <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
        <Default Extension="xml" ContentType="application/xml"/>
        <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
      </Types>`,
    );
    zip.file(
      "_rels/.rels",
      `<?xml version="1.0" encoding="UTF-8"?>
      <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
        <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
      </Relationships>`,
    );
    zip.file(
      "word/_rels/document.xml.rels",
      `<?xml version="1.0" encoding="UTF-8"?>
      <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>`,
    );
    zip.file(
      "word/document.xml",
      `<?xml version="1.0" encoding="UTF-8"?>
      <w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
        <w:body>
          <w:tbl>
            <w:tr>
              <w:tc><w:p><w:r><w:t>Header</w:t></w:r></w:p></w:tc>
            </w:tr>
            <w:tr>
              <w:tc>
                <w:p><w:r><w:t>Line 1</w:t></w:r></w:p>
                <w:p><w:r><w:t>Line 2</w:t></w:r></w:p>
              </w:tc>
            </w:tr>
          </w:tbl>
        </w:body>
      </w:document>`,
    );

    const buf = Buffer.from(await zip.generateAsync({ type: "nodebuffer" }));
    const result = await converter.convert(buf, { extension: ".docx" });

    // Table should not be broken — both lines in the same cell row
    // GFM uses trailing spaces + newline for in-cell line breaks
    const markdown = result.markdown;
    expect(markdown).toContain("Line 1");
    expect(markdown).toContain("Line 2");
    // The cell content should not produce separate table rows
    expect(markdown).not.toMatch(/\| Line 1[^|]*\|\n\| Line 2/);
  });

  test("text-only docs have no image references", async () => {
    const buffer = await buildDocx();
    const result = await converter.convert(buffer, { extension: ".docx" });
    expect(result.markdown).not.toContain("image");
  });
});
