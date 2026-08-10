/**
 * Native-accelerated Markit with transparent TS fallback.
 *
 * When the Rust native addon is available, all conversion runs through it
 * and the TypeScript implementation is never loaded (it costs ~100ms of
 * import time). When it is not — unsupported platform, or MARKIT_FORCE_TS=1
 * — the TS implementation is loaded eagerly at module load (top-level
 * await), so synchronous methods like accepts() behave identically either
 * way. The public API is the same in both modes.
 */

import { createRequire } from "node:module";
import type { ConversionResult, Converter, StreamInfo } from "./types.js";

// Re-export types so consumers can import everything from one place
export type { ConversionResult, Converter, StreamInfo } from "./types.js";

// ---------- native loader ----------

interface NativeBinding {
  Markit: new () => NativeMarkitInstance;
  converterAccepts(name: string, info: StreamInfo): boolean;
  converterConvert(
    name: string,
    input: Buffer,
    info: StreamInfo,
  ): Promise<ConversionResult>;
  converterConvertUrl(
    name: string,
    url: string,
  ): Promise<ConversionResult | null>;
}

interface NativeMarkitInstance {
  convert(input: Buffer, info?: StreamInfo): Promise<ConversionResult>;
  convertFile(
    path: string,
    extra?: Partial<StreamInfo>,
  ): Promise<ConversionResult>;
  convertUrl(url: string): Promise<ConversionResult>;
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

// ---------- TS fallback (loaded only when native is absent) ----------

type TsImpl = typeof import("./ts-impl.js");

const ts: TsImpl | null = native ? null : await import("./ts-impl.js");

/** The TS implementation; only callable in fallback mode. */
function tsImpl(): TsImpl {
  if (!ts) {
    throw new Error(
      "markit internal error: TS fallback requested while native is active",
    );
  }
  return ts;
}

// ---------- Markit class ----------

/**
 * Drop-in replacement for the TS Markit class.
 * Uses the native addon when available, falls back to TS.
 */
export class Markit {
  private _native: NativeMarkitInstance | null = null;
  private _ts: InstanceType<TsImpl["Markit"]> | null = null;

  constructor() {
    if (native) {
      this._native = new native.Markit();
    } else {
      this._ts = new (tsImpl().Markit)();
    }
  }

  async convert(
    input: Buffer,
    streamInfo: StreamInfo,
  ): Promise<ConversionResult> {
    if (this._native) {
      return this._native.convert(input, streamInfo);
    }
    return (this._ts as InstanceType<TsImpl["Markit"]>).convert(
      input,
      streamInfo,
    );
  }

  async convertFile(
    path: string,
    extra?: Partial<StreamInfo>,
  ): Promise<ConversionResult> {
    if (this._native) {
      return this._native.convertFile(path, extra);
    }
    return (this._ts as InstanceType<TsImpl["Markit"]>).convertFile(
      path,
      extra,
    );
  }

  async convertUrl(url: string): Promise<ConversionResult> {
    if (this._native) {
      return this._native.convertUrl(url);
    }
    return (this._ts as InstanceType<TsImpl["Markit"]>).convertUrl(url);
  }
}

// ---------- Converter wrappers ----------

/**
 * Build a converter class with the same shape as the TS original:
 * native-by-name when the addon is loaded, TS instance otherwise.
 * The TS fallback instance is created per wrapper instance, lazily,
 * and only ever in fallback mode.
 */
function makeConverterClass(converterName: string, tsClass: keyof TsImpl) {
  return class implements Converter {
    name = converterName;
    #ts: Converter | null = null;

    #tsFallback(): Converter {
      if (!this.#ts) {
        const Cls = tsImpl()[tsClass] as new () => Converter;
        this.#ts = new Cls();
      }
      return this.#ts;
    }

    accepts(streamInfo: StreamInfo): boolean {
      if (native) {
        return native.converterAccepts(converterName, streamInfo);
      }
      return this.#tsFallback().accepts(streamInfo);
    }

    async convert(
      input: Buffer,
      streamInfo: StreamInfo,
    ): Promise<ConversionResult> {
      if (native) {
        return native.converterConvert(converterName, input, streamInfo);
      }
      return this.#tsFallback().convert(input, streamInfo);
    }

    async convertUrl(url: string): Promise<ConversionResult> {
      if (native) {
        const result = await native.converterConvertUrl(converterName, url);
        if (result === null) {
          throw new Error(
            `Converter '${converterName}' does not support URL conversion`,
          );
        }
        return result;
      }
      const fallback = this.#tsFallback();
      if (!fallback.convertUrl) {
        throw new Error(
          `Converter '${converterName}' does not support URL conversion`,
        );
      }
      return fallback.convertUrl(url);
    }
  };
}

export const AudioConverter = makeConverterClass("audio", "AudioConverter");
export const CsvConverter = makeConverterClass("csv", "CsvConverter");
export const DocxConverter = makeConverterClass("docx", "DocxConverter");
export const EpubConverter = makeConverterClass("epub", "EpubConverter");
export const GitHubConverter = makeConverterClass("github", "GitHubConverter");
export const HtmlConverter = makeConverterClass("html", "HtmlConverter");
export const ImageConverter = makeConverterClass("image", "ImageConverter");
export const IpynbConverter = makeConverterClass("ipynb", "IpynbConverter");
export const IWorkConverter = makeConverterClass("iwork", "IWorkConverter");
export const JsonConverter = makeConverterClass("json", "JsonConverter");
export const PdfConverter = makeConverterClass("pdf", "PdfConverter");
export const PlainTextConverter = makeConverterClass(
  "plain-text",
  "PlainTextConverter",
);
export const PptxConverter = makeConverterClass("pptx", "PptxConverter");
export const RssConverter = makeConverterClass("rss", "RssConverter");
export const WikipediaConverter = makeConverterClass(
  "wikipedia",
  "WikipediaConverter",
);
export const XlsxConverter = makeConverterClass("xlsx", "XlsxConverter");
export const XmlConverter = makeConverterClass("xml", "XmlConverter");
export const YamlConverter = makeConverterClass("yaml", "YamlConverter");

/**
 * ZipConverter keeps the original constructor signature
 * `new ZipConverter(parentConverters)`. When a custom parent list is
 * supplied, conversion always uses the TS implementation (the native
 * registry only knows the builtins), which is imported on first use;
 * with no argument, the native path uses the builtin parent set.
 * accepts() delegates to native when loaded — its logic is identical
 * regardless of the parent set.
 */
export class ZipConverter implements Converter {
  name = "zip";
  private readonly parents: Converter[];
  private readonly custom: boolean;
  private _ts: Converter | null = null;

  constructor(parentConverters: Converter[] = []) {
    this.parents = parentConverters;
    this.custom = parentConverters.length > 0;
    if (!native) {
      this._ts = new (tsImpl().ZipConverter)(parentConverters);
    }
  }

  private async tsZip(): Promise<Converter> {
    if (!this._ts) {
      const { ZipConverter: TsZip } = await import("./converters/zip.js");
      this._ts = new TsZip(this.parents);
    }
    return this._ts;
  }

  accepts(streamInfo: StreamInfo): boolean {
    if (native && !this.custom) {
      return native.converterAccepts("zip", streamInfo);
    }
    if (this._ts) {
      return this._ts.accepts(streamInfo);
    }
    // Native mode with custom parents, TS zip not yet loaded: zip
    // acceptance depends only on the stream info, so native answers.
    return (native as NativeBinding).converterAccepts("zip", streamInfo);
  }

  async convert(
    input: Buffer,
    streamInfo: StreamInfo,
  ): Promise<ConversionResult> {
    if (native && !this.custom) {
      return native.converterConvert("zip", input, streamInfo);
    }
    return (await this.tsZip()).convert(input, streamInfo);
  }
}
