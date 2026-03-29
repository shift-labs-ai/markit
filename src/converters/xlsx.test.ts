import { describe, expect, test } from "bun:test";
import JSZip from "jszip";
import { XlsxConverter } from "./xlsx.js";

const converter = new XlsxConverter();

/**
 * Build a minimal XLSX buffer in memory.
 * XLSX is a ZIP containing XML files — we construct just enough
 * for the converter to parse: workbook, sheet, rels, and shared strings.
 */
async function buildXlsx(sharedStrings: string[]): Promise<Buffer> {
  const zip = new JSZip();

  // Shared strings XML
  const siEntries = sharedStrings.map((s) => `<si><t>${s}</t></si>`).join("");
  zip.file(
    "xl/sharedStrings.xml",
    `<?xml version="1.0" encoding="UTF-8"?>
    <sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
         count="${sharedStrings.length}" uniqueCount="${sharedStrings.length}">
      ${siEntries}
    </sst>`,
  );

  // Sheet with one row referencing each shared string
  const cells = sharedStrings
    .map((_, i) => `<c r="A${i + 1}" t="s"><v>${i}</v></c>`)
    .join("");
  zip.file(
    "xl/worksheets/sheet1.xml",
    `<?xml version="1.0" encoding="UTF-8"?>
    <worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
      <sheetData>
        <row r="1">${cells}</row>
      </sheetData>
    </worksheet>`,
  );

  // Workbook
  zip.file(
    "xl/workbook.xml",
    `<?xml version="1.0" encoding="UTF-8"?>
    <workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
              xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
      <sheets>
        <sheet name="Sheet1" sheetId="1" r:id="rId1"/>
      </sheets>
    </workbook>`,
  );

  // Workbook rels
  zip.file(
    "xl/_rels/workbook.xml.rels",
    `<?xml version="1.0" encoding="UTF-8"?>
    <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
      <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
                    Target="worksheets/sheet1.xml"/>
    </Relationships>`,
  );

  const buf = await zip.generateAsync({ type: "nodebuffer" });
  return Buffer.from(buf);
}

describe("XlsxConverter", () => {
  test("handles >1000 entity references without crashing", async () => {
    // Each string has an &amp; entity, so 2000 strings = 2000 entity refs.
    // Default fast-xml-parser limit is 1000 — this verifies our raised limit.
    const strings = Array.from({ length: 2000 }, (_, i) => `val&amp;ue_${i}`);
    const buffer = await buildXlsx(strings);

    const result = await converter.convert(buffer, { extension: ".xlsx" });
    expect(result.markdown).toContain("val&ue_0");
    expect(result.markdown).toContain("val&ue_999");
    expect(result.markdown).toContain("val&ue_1999");
  });

  test("handles shared strings with XML entities", async () => {
    const strings = [
      "AT&amp;T",
      "x &lt; y",
      "a &gt; b",
      "say &quot;hello&quot;",
      "it&apos;s",
    ];
    const buffer = await buildXlsx(strings);

    const result = await converter.convert(buffer, { extension: ".xlsx" });
    expect(result.markdown).toContain("AT&T");
    expect(result.markdown).toContain("x < y");
    expect(result.markdown).toContain("a > b");
    expect(result.markdown).toContain('say "hello"');
    expect(result.markdown).toContain("it's");
  });
});
