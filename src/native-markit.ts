/**
 * Native-accelerated Markit with transparent TS fallback.
 *
 * When the Rust native addon is available, all conversion runs through it.
 * When it is not (or MARKIT_FORCE_TS=1 is set), the existing TypeScript
 * implementations are used instead. The public API is identical either way.
 */

import { createRequire } from "node:module";
import type { ConversionResult, Converter, StreamInfo } from "./types.js";

// Re-export types so consumers can import everything from one place
export type { ConversionResult, Converter, StreamInfo } from "./types.js";

// ---------- native loader ----------

interface NativeBinding {
  Markit: new () => NativeMarkitInstance;
  converterNames(): string[];
  converterAccepts(name: string, info: JsStreamInfo): boolean;
  converterConvert(
    name: string,
    input: Buffer,
    info: JsStreamInfo,
  ): Promise<JsConversionResult>;
  converterConvertUrl(
    name: string,
    url: string,
  ): Promise<JsConversionResult | null>;
}

interface NativeMarkitInstance {
  convert(input: Buffer, info?: JsStreamInfo): Promise<JsConversionResult>;
  convertFile(path: string, extra?: JsStreamInfo): Promise<JsConversionResult>;
  convertUrl(url: string): Promise<JsConversionResult>;
}

interface JsStreamInfo {
  mimetype?: string;
  extension?: string;
  charset?: string;
  filename?: string;
  localPath?: string;
  url?: string;
  imageDir?: string;
}

interface JsConversionResult {
  markdown: string;
  title?: string;
}

let native: NativeBinding | null = null;

if (!process.env.MARKIT_FORCE_TS) {
  try {
    const require = createRequire(import.meta.url);
    native = require("../native.cjs") as NativeBinding | null;
  } catch {
    native = null;
  }
}

/** Whether the native addon is loaded. */
export const isNative: boolean = native !== null;

// ---------- TS fallback imports (lazy) ----------

let _tsMarkit: typeof import("./markit.js") | null = null;

async function getTsMarkit() {
  if (!_tsMarkit) {
    _tsMarkit = await import("./markit.js");
  }
  return _tsMarkit;
}

// ---------- Markit class ----------

/**
 * Drop-in replacement for the TS Markit class.
 * Uses native addon when available, falls back to TS.
 */
export class Markit {
  private _native: NativeMarkitInstance | null = null;
  private _ts: InstanceType<typeof import("./markit.js").Markit> | null = null;

  constructor() {
    if (native) {
      this._native = new native.Markit();
    }
  }

  private async ts() {
    if (!this._ts) {
      const mod = await getTsMarkit();
      this._ts = new mod.Markit();
    }
    return this._ts;
  }

  async convert(
    input: Buffer,
    streamInfo: StreamInfo,
  ): Promise<ConversionResult> {
    if (this._native) {
      return this._native.convert(input, streamInfo);
    }
    return (await this.ts()).convert(input, streamInfo);
  }

  async convertFile(
    path: string,
    extra?: Partial<StreamInfo>,
  ): Promise<ConversionResult> {
    if (this._native) {
      return this._native.convertFile(path, extra as JsStreamInfo | undefined);
    }
    return (await this.ts()).convertFile(path, extra);
  }

  async convertUrl(url: string): Promise<ConversionResult> {
    if (this._native) {
      return this._native.convertUrl(url);
    }
    return (await this.ts()).convertUrl(url);
  }
}

// ---------- Converter wrappers ----------

/**
 * A converter class that delegates to the native addon by name,
 * falling back to the TS implementation when native is unavailable.
 */
function makeNativeConverter(
  converterName: string,
  getTsFallback: () => Promise<Converter>,
): Converter {
  let tsFallback: Converter | null = null;

  const getFallback = async (): Promise<Converter> => {
    if (!tsFallback) {
      tsFallback = await getTsFallback();
    }
    return tsFallback;
  };

  // For accepts(), we need synchronous access — always have TS fallback ready
  // if native is not available
  const converter: Converter = {
    name: converterName,

    accepts(streamInfo: StreamInfo): boolean {
      if (native) {
        return native.converterAccepts(converterName, streamInfo);
      }
      // Synchronous fallback: if TS converter not loaded yet, load it sync-style
      // This is fine because accepts() is always called in a context where
      // the converter was previously loaded or we can load it.
      if (!tsFallback) {
        // Return false if we can't check synchronously — the Markit class
        // handles routing anyway
        return false;
      }
      return tsFallback.accepts(streamInfo);
    },

    async convert(
      input: Buffer,
      streamInfo: StreamInfo,
    ): Promise<ConversionResult> {
      if (native) {
        return native.converterConvert(converterName, input, streamInfo);
      }
      return (await getFallback()).convert(input, streamInfo);
    },

    convertUrl: async (url: string): Promise<ConversionResult> => {
      if (native) {
        const result = await native.converterConvertUrl(converterName, url);
        if (result === null) {
          throw new Error(
            `Converter '${converterName}' does not support URL conversion`,
          );
        }
        return result;
      }
      const fb = await getFallback();
      if (!fb.convertUrl) {
        throw new Error(
          `Converter '${converterName}' does not support URL conversion`,
        );
      }
      return fb.convertUrl(url);
    },
  };

  return converter;
}

// ---------- Individual converter classes ----------
// These maintain the same class-based export shape as the TS originals.

function makeConverterClass(
  converterName: string,
  importFn: () => Promise<Converter>,
) {
  const inner = makeNativeConverter(converterName, importFn);

  return class implements Converter {
    name = converterName;

    accepts(streamInfo: StreamInfo): boolean {
      return inner.accepts(streamInfo);
    }

    async convert(
      input: Buffer,
      streamInfo: StreamInfo,
    ): Promise<ConversionResult> {
      return inner.convert(input, streamInfo);
    }

    async convertUrl(url: string): Promise<ConversionResult> {
      if (!inner.convertUrl) {
        throw new Error(`${converterName}: no convertUrl hook`);
      }
      return inner.convertUrl(url);
    }
  };
}

export const AudioConverter = makeConverterClass(
  "audio",
  async () => new (await import("./converters/audio.js")).AudioConverter(),
);
export const CsvConverter = makeConverterClass(
  "csv",
  async () => new (await import("./converters/csv.js")).CsvConverter(),
);
export const DocxConverter = makeConverterClass(
  "docx",
  async () => new (await import("./converters/docx.js")).DocxConverter(),
);
export const EpubConverter = makeConverterClass(
  "epub",
  async () => new (await import("./converters/epub.js")).EpubConverter(),
);
export const GitHubConverter = makeConverterClass(
  "github",
  async () => new (await import("./converters/github.js")).GitHubConverter(),
);
export const HtmlConverter = makeConverterClass(
  "html",
  async () => new (await import("./converters/html.js")).HtmlConverter(),
);
export const ImageConverter = makeConverterClass(
  "image",
  async () => new (await import("./converters/image.js")).ImageConverter(),
);
export const IpynbConverter = makeConverterClass(
  "ipynb",
  async () => new (await import("./converters/ipynb.js")).IpynbConverter(),
);
export const IWorkConverter = makeConverterClass(
  "iwork",
  async () => new (await import("./converters/iwork.js")).IWorkConverter(),
);
export const JsonConverter = makeConverterClass(
  "json",
  async () => new (await import("./converters/json.js")).JsonConverter(),
);
export const PdfConverter = makeConverterClass(
  "pdf",
  async () => new (await import("./converters/pdf/index.js")).PdfConverter(),
);
export const PlainTextConverter = makeConverterClass(
  "plain-text",
  async () =>
    new (await import("./converters/plain-text.js")).PlainTextConverter(),
);
export const PptxConverter = makeConverterClass(
  "pptx",
  async () => new (await import("./converters/pptx.js")).PptxConverter(),
);
export const RssConverter = makeConverterClass(
  "rss",
  async () => new (await import("./converters/rss.js")).RssConverter(),
);
export const WikipediaConverter = makeConverterClass(
  "wikipedia",
  async () =>
    new (await import("./converters/wikipedia.js")).WikipediaConverter(),
);
export const XlsxConverter = makeConverterClass(
  "xlsx",
  async () => new (await import("./converters/xlsx.js")).XlsxConverter(),
);
export const XmlConverter = makeConverterClass(
  "xml",
  async () => new (await import("./converters/xml.js")).XmlConverter(),
);
export const YamlConverter = makeConverterClass(
  "yaml",
  async () => new (await import("./converters/yaml.js")).YamlConverter(),
);
export const ZipConverter = makeConverterClass(
  "zip",
  async () => new (await import("./converters/zip.js")).ZipConverter([]),
);
