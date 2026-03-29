import { describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import JSZip from "jszip";
import { PptxConverter } from "./pptx.js";

const converter = new PptxConverter();

// 1x1 red PNG
const TINY_PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg==",
  "base64",
);

/**
 * Build a minimal PPTX with one slide containing a text shape and optionally an image.
 */
async function buildPptx(opts?: {
  image?: boolean;
  imageName?: string;
}): Promise<Buffer> {
  const zip = new JSZip();

  const hasImage = opts?.image ?? false;
  const imageName = opts?.imageName ?? "Picture 1";

  // Presentation
  zip.file(
    "ppt/presentation.xml",
    `<?xml version="1.0" encoding="UTF-8"?>
    <p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
                    xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
      <p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst>
    </p:presentation>`,
  );

  // Presentation rels
  zip.file(
    "ppt/_rels/presentation.xml.rels",
    `<?xml version="1.0" encoding="UTF-8"?>
    <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
      <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide"
                    Target="slides/slide1.xml"/>
    </Relationships>`,
  );

  // Slide with text shape + optional pic
  const picXml = hasImage
    ? `<p:pic>
        <p:nvPicPr>
          <p:cNvPr id="4" name="${imageName}"/>
          <p:cNvPicPr/>
          <p:nvPr/>
        </p:nvPicPr>
        <p:blipFill>
          <a:blip xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" r:embed="rId2"/>
        </p:blipFill>
        <p:spPr/>
      </p:pic>`
    : "";

  zip.file(
    "ppt/slides/slide1.xml",
    `<?xml version="1.0" encoding="UTF-8"?>
    <p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
           xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
           xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
      <p:cSld>
        <p:spTree>
          <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
          <p:grpSpPr/>
          <p:sp>
            <p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
            <p:spPr/>
            <p:txBody>
              <a:p><a:r><a:t>Hello World</a:t></a:r></a:p>
            </p:txBody>
          </p:sp>
          ${picXml}
        </p:spTree>
      </p:cSld>
    </p:sld>`,
  );

  // Slide rels (image ref if needed)
  if (hasImage) {
    zip.file(
      "ppt/slides/_rels/slide1.xml.rels",
      `<?xml version="1.0" encoding="UTF-8"?>
      <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
        <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image"
                      Target="../media/image1.png"/>
      </Relationships>`,
    );
    zip.file("ppt/media/image1.png", TINY_PNG);
  }

  const buf = await zip.generateAsync({ type: "nodebuffer" });
  return Buffer.from(buf);
}

describe("PptxConverter", () => {
  test("extracts text from shapes", async () => {
    const buffer = await buildPptx();
    const result = await converter.convert(buffer, { extension: ".pptx" });
    expect(result.markdown).toContain("Hello World");
  });

  test("emits image placeholder comment without imageDir", async () => {
    const buffer = await buildPptx({ image: true, imageName: "Logo" });
    const result = await converter.convert(buffer, { extension: ".pptx" });
    expect(result.markdown).toContain("<!-- image: Logo (slide 1) -->");
  });

  test("saves image to disk with imageDir", async () => {
    const dir = mkdtempSync(join(tmpdir(), "pptx-test-"));
    try {
      const buffer = await buildPptx({ image: true });
      const result = await converter.convert(buffer, {
        extension: ".pptx",
        imageDir: dir,
      });
      expect(result.markdown).toContain("![Picture 1]");
      expect(result.markdown).toContain(dir);

      // Verify file was written
      const files = Bun.file(join(dir, "slide1_1.png"));
      expect(await files.exists()).toBe(true);
    } finally {
      rmSync(dir, { recursive: true });
    }
  });

  test("text-only slides have no image references", async () => {
    const buffer = await buildPptx();
    const result = await converter.convert(buffer, { extension: ".pptx" });
    expect(result.markdown).not.toContain("image");
  });
});
