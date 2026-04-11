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

const USER_AGENT = "markit/0.1.0";

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
        "User-Agent": USER_AGENT,
      },
    });

    if (!response.ok) {
      throw new Error(
        `Failed to fetch ${url}: ${response.status} ${response.statusText}`,
      );
    }

    const contentType = response.headers.get("content-type") || "";
    const mimetype = contentType.split(";")[0].trim();
    const urlPath = new URL(url).pathname;
    const ext = extname(urlPath).toLowerCase();

    // Content negotiation worked — server returned markdown directly
    if (mimetype === "text/markdown") {
      const buffer = Buffer.from(await response.arrayBuffer());
      return this.convert(buffer, {
        url,
        mimetype: "text/markdown",
        extension: ".md",
        filename: basename(urlPath) || undefined,
      });
    }

    const buffer = Buffer.from(await response.arrayBuffer());

    // For HTML responses, try to discover a raw markdown source.
    // Patterns: <link rel="alternate">, VitePress .md files, llms.txt convention.
    if (mimetype === "text/html") {
      const result = await this.tryMarkdownSource(buffer, url, ext);
      if (result) return result;
    }

    return this.convert(buffer, {
      url,
      mimetype,
      extension: ext || undefined,
      filename: basename(urlPath) || undefined,
    });
  }

  /**
   * Inspect an HTML response for a discoverable markdown source URL.
   * If found, fetch and convert the raw markdown instead.
   */
  private async tryMarkdownSource(
    htmlBuffer: Buffer,
    url: string,
    ext: string,
  ): Promise<ConversionResult | null> {
    const html = htmlBuffer.toString(
      "utf-8",
      0,
      Math.min(htmlBuffer.length, 50_000),
    );
    const mdSourceUrl = discoverMarkdownSource(html, url, ext);
    if (!mdSourceUrl) return null;

    try {
      const response = await fetch(mdSourceUrl, {
        headers: { "User-Agent": USER_AGENT },
      });
      if (!response.ok) return null;

      const ct = (response.headers.get("content-type") || "")
        .split(";")[0]
        .trim();
      if (!ct.includes("markdown") && !ct.includes("text/plain")) return null;

      const mdBuffer = Buffer.from(await response.arrayBuffer());
      return this.convert(mdBuffer, {
        url: mdSourceUrl,
        mimetype: "text/markdown",
        extension: ".md",
        filename: basename(new URL(mdSourceUrl).pathname),
      });
    } catch {
      return null;
    }
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

/**
 * Try to discover a raw markdown source URL from an HTML response.
 * Checks multiple patterns:
 *   1. <link rel="alternate" type="text/markdown" href="..."> tag
 *   2. VitePress markers → append .md to the URL
 *   3. llms.txt convention → try url.md or url.html.md
 *
 * @internal Exported for testing.
 */
export function discoverMarkdownSource(
  html: string,
  url: string,
  ext: string,
): string | null {
  // 1. Look for <link rel="alternate" type="text/markdown" href="...">
  const linkMatch =
    html.match(
      /<link[^>]+rel=["']alternate["'][^>]+type=["']text\/markdown["'][^>]+href=["']([^"']+)["']/i,
    ) ??
    html.match(
      /<link[^>]+type=["']text\/markdown["'][^>]+rel=["']alternate["'][^>]+href=["']([^"']+)["']/i,
    );
  if (linkMatch?.[1]) {
    try {
      return new URL(linkMatch[1], url).href;
    } catch {
      /* ignore malformed URLs */
    }
  }

  // 2. VitePress detection — serves .md alongside HTML
  const isVitePress =
    html.includes("__VP_HASH_MAP__") ||
    html.includes("VPContent") ||
    html.includes("vitepress");

  // 3. llms.txt convention: try url.md for extensionless URLs
  const hasLlmsTxt = html.includes("llms.txt");

  if (!ext && (isVitePress || hasLlmsTxt)) {
    return appendMdExtension(url);
  }

  return null;
}

function appendMdExtension(url: string): string {
  return url.endsWith("/") ? `${url.slice(0, -1)}.md` : `${url}.md`;
}
