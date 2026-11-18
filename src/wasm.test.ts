import { describe, expect, test } from "bun:test";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { crc32, deflateRawSync } from "node:zlib";
import { Markit } from "./index.js";
import type { StreamInfo } from "./types.js";

// The browser build is the same engine compiled to wasm32, so its output
// must match the native addon's byte for byte. Run `bun run build:wasm`
// first; without its output this suite is skipped.
const pkg = new URL("../npm/wasm/", import.meta.url);
const built = existsSync(new URL("markit_bg.wasm", pkg));

/** A deflate ZIP, the container behind docx/xlsx/pptx/epub/iWork. */
function zip(entries: Record<string, string>): Buffer {
  const count = Object.keys(entries).length;
  const local: Buffer[] = [];
  const central: Buffer[] = [];
  let offset = 0;
  for (const [name, text] of Object.entries(entries)) {
    const raw = Buffer.from(text);
    const data = deflateRawSync(raw);
    const fields = Buffer.alloc(16);
    fields.writeUInt32LE(crc32(raw), 0);
    fields.writeUInt32LE(data.length, 4);
    fields.writeUInt32LE(raw.length, 8);
    fields.writeUInt16LE(name.length, 12);
    const head = Buffer.from([0x14, 0, 0, 0, 8, 0, 0, 0, 0, 0]);
    local.push(
      Buffer.from("PK\x03\x04", "latin1"),
      head,
      fields,
      Buffer.from(name),
      data,
    );
    // comment length, disk, attributes, then the local header's offset
    const tail = Buffer.alloc(14);
    tail.writeUInt32LE(offset, 10);
    central.push(
      Buffer.from("PK\x01\x02\x14\x00", "latin1"),
      head,
      fields,
      tail,
      Buffer.from(name),
    );
    offset += 30 + name.length + data.length;
  }
  const dir = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.write("PK\x05\x06", "latin1");
  end.writeUInt16LE(count, 8);
  end.writeUInt16LE(count, 10);
  end.writeUInt32LE(dir.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...local, dir, end]);
}

const REL = "http://schemas.openxmlformats.org/package/2006/relationships";
const OFFICE =
  "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const SHEET = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const WORD = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const SLIDES = "http://schemas.openxmlformats.org/presentationml/2006/main";
const DRAWING = "http://schemas.openxmlformats.org/drawingml/2006/main";
const APPLE = "http://developer.apple.com/namespaces";

const docx = zip({
  "_rels/.rels": `<Relationships xmlns="${REL}"><Relationship Id="rId1" Type="${OFFICE}/officeDocument" Target="word/document.xml"/></Relationships>`,
  "word/document.xml": `<w:document xmlns:w="${WORD}"><w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Report</w:t></w:r></w:p><w:p><w:r><w:t>Body &amp; text</w:t></w:r></w:p></w:body></w:document>`,
  "word/_rels/document.xml.rels": `<Relationships xmlns="${REL}"/>`,
});

const xlsx = zip({
  "xl/workbook.xml": `<workbook xmlns="${SHEET}" xmlns:r="${OFFICE}"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>`,
  "xl/_rels/workbook.xml.rels": `<Relationships xmlns="${REL}"><Relationship Id="rId1" Target="worksheets/sheet1.xml" Type="${OFFICE}/worksheet"/></Relationships>`,
  "xl/sharedStrings.xml": `<sst xmlns="${SHEET}"><si><t>name</t></si><si><t>a</t></si></sst>`,
  "xl/worksheets/sheet1.xml": `<worksheet xmlns="${SHEET}"><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1"><v>1</v></c></row><row r="2"><c r="A2" t="s"><v>1</v></c><c r="B2"><v>2.5</v></c></row></sheetData></worksheet>`,
});

const pptx = zip({
  "ppt/presentation.xml": `<p:presentation xmlns:p="${SLIDES}" xmlns:r="${OFFICE}"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst></p:presentation>`,
  "ppt/_rels/presentation.xml.rels": `<Relationships xmlns="${REL}"><Relationship Id="rId1" Type="${OFFICE}/slide" Target="slides/slide1.xml"/></Relationships>`,
  "ppt/slides/slide1.xml": `<p:sld xmlns:p="${SLIDES}" xmlns:a="${DRAWING}"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr/><p:txBody><a:p><a:r><a:t>Hello World</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>`,
});

const epub = zip({
  "META-INF/container.xml": `<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>`,
  "content.opf": `<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/"><metadata><dc:title>Book</dc:title><dc:creator>Ann</dc:creator></metadata><manifest><item id="ch0" href="ch0.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="ch0"/></spine></package>`,
  "ch0.xhtml": "<html><body><h1>One</h1><p>First chapter.</p></body></html>",
});

const pages = zip({
  "index.xml": `<sl:document xmlns:sf="${APPLE}/sf" xmlns:sl="${APPLE}/sl"><sf:section><sf:layout><sf:p sf:style="text-20-paragraphstyle-Body 1">Hello from Pages</sf:p></sf:layout></sf:section></sl:document>`,
});

/** A JPEG that is only an EXIF block naming the camera make. */
function jpeg(make: string): Buffer {
  const tiff = Buffer.alloc(26 + make.length + 1);
  tiff.write("II*\0", "latin1"); // little-endian TIFF
  tiff.writeUInt32LE(8, 4); // IFD0 offset
  tiff.writeUInt16LE(1, 8); // one entry: Make, ASCII, length, value offset
  tiff.writeUInt16LE(0x010f, 10);
  tiff.writeUInt16LE(2, 12);
  tiff.writeUInt32LE(make.length + 1, 14);
  tiff.writeUInt32LE(26, 18);
  tiff.write(make, 26, "latin1");
  const exif = Buffer.concat([Buffer.from("Exif\0\0", "latin1"), tiff]);
  const app1 = Buffer.from([0xff, 0xe1, 0, 0]);
  app1.writeUInt16BE(exif.length + 2, 2);
  return Buffer.concat([
    Buffer.from([0xff, 0xd8]),
    app1,
    exif,
    Buffer.from([0xff, 0xd9]),
  ]);
}

/** Ten samples of 8-bit mono PCM silence at 44.1 kHz. */
function wav(): Buffer {
  const out = Buffer.alloc(54, 0x80);
  out.write("RIFF", 0, "latin1");
  out.writeUInt32LE(46, 4);
  out.write("WAVEfmt ", 8, "latin1");
  out.writeUInt32LE(16, 16); // fmt chunk size
  out.writeUInt16LE(1, 20); // PCM
  out.writeUInt16LE(1, 22); // channels
  out.writeUInt32LE(44100, 24); // sample rate
  out.writeUInt32LE(44100, 28); // byte rate
  out.writeUInt16LE(1, 32); // block align
  out.writeUInt16LE(8, 34); // bits per sample
  out.write("data", 36, "latin1");
  out.writeUInt32LE(10, 40);
  return out;
}

const text = (s: string) => Buffer.from(s);

const samples: [string, Buffer, StreamInfo][] = [
  ["docx", docx, { extension: ".docx" }],
  ["xlsx", xlsx, { extension: ".xlsx" }],
  ["pptx", pptx, { extension: ".pptx" }],
  ["epub", epub, { extension: ".epub" }],
  ["pages", pages, { extension: ".pages" }],
  ["jpeg", jpeg("Canon"), { extension: ".jpg", filename: "photo.jpg" }],
  ["wav", wav(), { extension: ".wav", filename: "clip.wav" }],
  [
    "zip",
    zip({ "a.csv": "x,y\n1,2\n", "b.md": "# B\n" }),
    { extension: ".zip" },
  ],
  [
    "html",
    text(
      "<html><head><title>T</title><script>x()</script></head><body><h1>Hi</h1><p>a &amp; <b>b</b></p></body></html>",
    ),
    { extension: ".html" },
  ],
  [
    "wikipedia",
    text(
      '<html><body><div id="mw-content-text"><p>Rust is a language.</p></div></body></html>',
    ),
    { url: "https://en.wikipedia.org/wiki/Rust" },
  ],
  ["csv", text("name,value\na,1\n"), { extension: ".csv" }],
  ["json", text('{"a":[1,2]}'), { extension: ".json" }],
  ["yaml", text("a: 1\n"), { extension: ".yaml" }],
  ["xml", text("<a><b/></a>"), { extension: ".xml" }],
  [
    "rss",
    text(
      '<rss version="2.0"><channel><title>Feed</title><item><title>One</title><pubDate>Mon, 06 Jan 2025 00:00:00 GMT</pubDate></item></channel></rss>',
    ),
    { extension: ".rss" },
  ],
  [
    "ipynb",
    text(
      '{"cells":[{"cell_type":"markdown","source":["# Nb"]},{"cell_type":"code","source":["1+1"],"outputs":[]}],"metadata":{},"nbformat":4}',
    ),
    { extension: ".ipynb" },
  ],
  ["code", text("print('hi')\n"), { extension: ".py" }],
  ["no stream info", text("plain\n"), {}],
  ["broken xlsx", text("not a zip"), { extension: ".xlsx" }],
];

for (const dir of [
  "../rust/testdata/pdf/regression/",
  "../test/fixtures/pdfs/encrypted/",
]) {
  const url = new URL(dir, import.meta.url);
  for (const name of readdirSync(url).filter((f) => f.endsWith(".pdf"))) {
    samples.push([
      name,
      readFileSync(new URL(name, url)),
      { extension: ".pdf" },
    ]);
  }
}

describe.skipIf(!built)("wasm build", async () => {
  const wasm = built ? await import(new URL("markit.js", pkg).href) : null;
  wasm?.initSync({ module: readFileSync(new URL("markit_bg.wasm", pkg)) });
  const native = new Markit();

  // Same result, or the same error: encrypted PDFs without a password and
  // broken archives must fail in wasm exactly as they do natively.
  test.each(
    samples,
  )("matches the native engine: %s", async (_, input, info) => {
    const expected = await native.convert(input, info).catch((e: Error) => e);
    const convert = () => wasm.convert(new Uint8Array(input), info);
    if (expected instanceof Error) {
      expect(convert).toThrow(expected.message);
    } else {
      expect(convert()).toEqual(expected);
    }
  });

  test("info is optional", () => {
    expect(wasm.convert(text("hello\n")).markdown).toBe("hello\n");
  });
});
