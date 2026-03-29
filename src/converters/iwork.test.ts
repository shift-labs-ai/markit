import { describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import JSZip from "jszip";
import { IWorkConverter } from "./iwork.js";

const converter = new IWorkConverter();

// 1x1 red PNG
const TINY_PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg==",
  "base64",
);

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

async function buildPages(opts?: {
  style?: string;
  image?: boolean;
}): Promise<Buffer> {
  const zip = new JSZip();
  const style = opts?.style ?? "text-20-paragraphstyle-Body 1";

  const xml = `<?xml version="1.0"?>
<sl:document xmlns:sfa="http://developer.apple.com/namespaces/sfa"
             xmlns:sf="http://developer.apple.com/namespaces/sf"
             xmlns:sl="http://developer.apple.com/namespaces/sl">
  <sf:section>
    <sf:layout>
      <sf:p sf:style="${style}">Hello from Pages</sf:p>
    </sf:layout>
  </sf:section>
</sl:document>`;

  zip.file("index.xml", xml);
  if (opts?.image) {
    zip.file("media/photo.png", TINY_PNG);
  }
  const buf = await zip.generateAsync({ type: "nodebuffer" });
  return Buffer.from(buf);
}

describe("Pages", () => {
  test("extracts body text", async () => {
    const buffer = await buildPages();
    const result = await converter.convert(buffer, { extension: ".pages" });
    expect(result.markdown).toContain("Hello from Pages");
  });

  test("detects title style", async () => {
    const buffer = await buildPages({
      style: "text-0-paragraphstyle-Title",
    });
    const result = await converter.convert(buffer, { extension: ".pages" });
    expect(result.markdown).toContain("# Hello from Pages");
  });

  test("detects heading styles", async () => {
    const buffer = await buildPages({
      style: "text-11-paragraphstyle-Heading 1",
    });
    const result = await converter.convert(buffer, { extension: ".pages" });
    expect(result.markdown).toContain("## Hello from Pages");
  });

  test("extracts images to imageDir", async () => {
    const dir = mkdtempSync(join(tmpdir(), "pages-test-"));
    try {
      const buffer = await buildPages({ image: true });
      const result = await converter.convert(buffer, {
        extension: ".pages",
        imageDir: dir,
      });
      expect(result.markdown).toContain("![photo.png]");
      const file = Bun.file(join(dir, "photo.png"));
      expect(await file.exists()).toBe(true);
    } finally {
      rmSync(dir, { recursive: true });
    }
  });

  test("emits image placeholder without imageDir", async () => {
    const buffer = await buildPages({ image: true });
    const result = await converter.convert(buffer, { extension: ".pages" });
    expect(result.markdown).toContain("<!-- image: photo.png -->");
  });
});

// ---------------------------------------------------------------------------
// Keynote
// ---------------------------------------------------------------------------

async function buildKeynote(): Promise<Buffer> {
  const zip = new JSZip();

  zip.file(
    "index.apxl",
    `<?xml version="1.0"?>
<key:presentation xmlns:sfa="http://developer.apple.com/namespaces/sfa"
                  xmlns:sf="http://developer.apple.com/namespaces/sf"
                  xmlns:key="http://developer.apple.com/namespaces/keynote2">
  <key:slide-list>
    <key:slide>
      <key:title-placeholder>
        <sf:p>Slide Title</sf:p>
      </key:title-placeholder>
      <key:body-placeholder>
        <sf:p>Body content here</sf:p>
      </key:body-placeholder>
    </key:slide>
    <key:slide>
      <key:title-placeholder>
        <sf:p>Second Slide</sf:p>
      </key:title-placeholder>
    </key:slide>
  </key:slide-list>
</key:presentation>`,
  );

  const buf = await zip.generateAsync({ type: "nodebuffer" });
  return Buffer.from(buf);
}

describe("Keynote", () => {
  test("extracts slide text", async () => {
    const buffer = await buildKeynote();
    const result = await converter.convert(buffer, { extension: ".key" });
    expect(result.markdown).toContain("# Slide Title");
    expect(result.markdown).toContain("Body content here");
    expect(result.markdown).toContain("<!-- Slide 1 -->");
    expect(result.markdown).toContain("<!-- Slide 2 -->");
    expect(result.markdown).toContain("# Second Slide");
  });

  test("sets title from first slide", async () => {
    const buffer = await buildKeynote();
    const result = await converter.convert(buffer, { extension: ".key" });
    expect(result.title).toBe("Slide Title");
  });
});

// ---------------------------------------------------------------------------
// Numbers
// ---------------------------------------------------------------------------

async function buildNumbers(numcols: number, cells: string): Promise<Buffer> {
  const zip = new JSZip();

  zip.file(
    "index.xml",
    `<?xml version="1.0"?>
<sl:document xmlns:sfa="http://developer.apple.com/namespaces/sfa"
             xmlns:sf="http://developer.apple.com/namespaces/sf"
             xmlns:sl="http://developer.apple.com/namespaces/sl">
  <sf:grid sf:numcols="${numcols}" sf:numrows="10">
    <sf:datasource>${cells}</sf:datasource>
  </sf:grid>
</sl:document>`,
  );

  const buf = await zip.generateAsync({ type: "nodebuffer" });
  return Buffer.from(buf);
}

describe("Numbers", () => {
  test("extracts table with correct columns", async () => {
    const cells = `
      <sf:t sf:ct="0" sf:f="0" sf:s="s1"><sf:ct sfa:s="Name"/></sf:t>
      <sf:t sf:f="0" sf:s="s1"><sf:ct sfa:s="Score"/></sf:t>
      <sf:t sf:ct="255" sf:f="0" sf:s="s2"><sf:ct sfa:s="Alice"/></sf:t>
      <sf:n sf:f="0" sf:s="s3" sf:v="95"/>
      <sf:t sf:ct="255" sf:f="0" sf:s="s2"><sf:ct sfa:s="Bob"/></sf:t>
      <sf:n sf:f="0" sf:s="s3" sf:v="87"/>`;

    const buffer = await buildNumbers(2, cells);
    const result = await converter.convert(buffer, { extension: ".numbers" });
    expect(result.markdown).toContain("| Name | Score |");
    expect(result.markdown).toContain("| Alice | 95 |");
    expect(result.markdown).toContain("| Bob | 87 |");
  });

  test("handles oversized default grid", async () => {
    const cells = `
      <sf:t sf:ct="0" sf:f="0" sf:s="s1"><sf:ct sfa:s="Name"/></sf:t>
      <sf:t sf:f="0" sf:s="s1"><sf:ct sfa:s="Score"/></sf:t>
      <sf:t sf:ct="255" sf:f="0" sf:s="s2"><sf:ct sfa:s="Alice"/></sf:t>
      <sf:n sf:f="0" sf:s="s3" sf:v="95"/>`;

    // numcols=7 but only 4 cells — should detect 2 columns
    const buffer = await buildNumbers(7, cells);
    const result = await converter.convert(buffer, { extension: ".numbers" });
    expect(result.markdown).toContain("| Name | Score |");
    expect(result.markdown).toContain("| Alice | 95 |");
  });
});
