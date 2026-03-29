import { readFileSync } from "node:fs";
import { basename, extname } from "node:path";
import { AudioConverter } from "./converters/audio.js";
import { CsvConverter } from "./converters/csv.js";
import { DocxConverter } from "./converters/docx.js";
import { EpubConverter } from "./converters/epub.js";
import { GitHubConverter } from "./converters/github.js";
import { HtmlConverter } from "./converters/html.js";
import { ImageConverter } from "./converters/image.js";
import { IpynbConverter } from "./converters/ipynb.js";
import { IWorkConverter } from "./converters/iwork.js";
import { JsonConverter } from "./converters/json.js";
import { PdfConverter } from "./converters/pdf/index.js";
import { PlainTextConverter } from "./converters/plain-text.js";
import { PptxConverter } from "./converters/pptx.js";
import { RssConverter } from "./converters/rss.js";
import { WikipediaConverter } from "./converters/wikipedia.js";
import { XlsxConverter } from "./converters/xlsx.js";
import { XmlConverter } from "./converters/xml.js";
import { YamlConverter } from "./converters/yaml.js";
import { ZipConverter } from "./converters/zip.js";
import type { PluginDef } from "./plugins/types.js";
import type {
  ConversionResult,
  Converter,
  MarkitOptions,
  StreamInfo,
} from "./types.js";

export class Markit {
  private converters: Converter[] = [];
  private options: MarkitOptions;

  constructor(options: MarkitOptions = {}, plugins: PluginDef[] = []) {
    this.options = options;

    // Plugin converters go first — they override builtins for the same format
    const pluginConverters = plugins.flatMap((p) => p.converters);

    // Built-in converters: specific formats first, generic last.
    const specific: Converter[] = [
      new PdfConverter(),
      new DocxConverter(),
      new PptxConverter(),
      new XlsxConverter(),
      new EpubConverter(),
      new IpynbConverter(),
      new IWorkConverter(),
      new GitHubConverter(),
      new WikipediaConverter(),
      new RssConverter(),
      new CsvConverter(),
      new JsonConverter(),
      new YamlConverter(),
      new ImageConverter(),
      new AudioConverter(),
    ];

    const generic: Converter[] = [new XmlConverter(), new HtmlConverter()];

    // ZIP gets all converters (plugin + builtin) for recursive extraction
    const allNonZip = [...pluginConverters, ...specific, ...generic];
    const zipConverter = new ZipConverter(allNonZip);

    // Plugin converters first, then builtins, plain text last
    this.converters = [
      ...pluginConverters,
      ...specific,
      zipConverter,
      ...generic,
      new PlainTextConverter(),
    ];
  }

  /**
   * Convert a local file to markdown.
   */
  async convertFile(
    path: string,
    extra?: Partial<StreamInfo>,
  ): Promise<ConversionResult> {
    const buffer = readFileSync(path);
    const streamInfo: StreamInfo = {
      localPath: path,
      extension: extname(path).toLowerCase(),
      filename: basename(path),
      ...extra,
    };
    return this.convert(buffer, streamInfo);
  }

  /**
   * Convert a URL to markdown.
   */
  async convertUrl(url: string): Promise<ConversionResult> {
    // Let converters with a URL-specific hook handle it first
    const streamInfo: StreamInfo = { url };
    for (const converter of this.converters) {
      if (!converter.convertUrl || !converter.accepts(streamInfo)) continue;
      try {
        return await converter.convertUrl(url, this.options);
      } catch {
        // Fall through to default fetch path
      }
    }

    const response = await fetch(url, {
      headers: {
        Accept: "text/markdown, text/html;q=0.9, text/plain;q=0.8, */*;q=0.1",
        "User-Agent": "mill/0.1.0",
      },
    });

    if (!response.ok) {
      throw new Error(
        `Failed to fetch ${url}: ${response.status} ${response.statusText}`,
      );
    }

    const contentType = response.headers.get("content-type") || "";
    const [mimetype] = contentType.split(";");

    // Derive extension from URL path
    const urlPath = new URL(url).pathname;
    const ext = extname(urlPath).toLowerCase();

    const buffer = Buffer.from(await response.arrayBuffer());
    const fetchedInfo: StreamInfo = {
      url,
      mimetype: mimetype.trim(),
      extension: ext || undefined,
      filename: basename(urlPath) || undefined,
    };

    return this.convert(buffer, fetchedInfo);
  }

  /**
   * Convert a buffer with stream info to markdown.
   */
  async convert(
    input: Buffer,
    streamInfo: StreamInfo,
  ): Promise<ConversionResult> {
    const errors: Array<{ converter: string; error: Error }> = [];

    for (const converter of this.converters) {
      if (!converter.accepts(streamInfo)) continue;

      try {
        return await converter.convert(input, streamInfo, this.options);
      } catch (err) {
        errors.push({
          converter: converter.name,
          error: err instanceof Error ? err : new Error(String(err)),
        });
      }
    }

    if (errors.length > 0) {
      const details = errors
        .map((e) => `  ${e.converter}: ${e.error.message}`)
        .join("\n");
      throw new Error(`Conversion failed:\n${details}`);
    }

    throw new Error(
      `Unsupported format: ${streamInfo.extension || streamInfo.mimetype || "unknown"}`,
    );
  }
}
