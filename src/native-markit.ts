/**
 * Native-accelerated Markit with transparent TS fallback.
 *
 * When the Rust native addon is available, all conversion runs through it.
 * When it is not (or MARKIT_FORCE_TS=1 is set), the existing TypeScript
 * implementations are used instead. The public API is identical either way.
 */

import { createRequire } from "node:module";
import { AudioConverter as TsAudioConverter } from "./converters/audio.js";
import { CsvConverter as TsCsvConverter } from "./converters/csv.js";
import { DocxConverter as TsDocxConverter } from "./converters/docx.js";
import { EpubConverter as TsEpubConverter } from "./converters/epub.js";
import { GitHubConverter as TsGitHubConverter } from "./converters/github.js";
import { HtmlConverter as TsHtmlConverter } from "./converters/html.js";
import { ImageConverter as TsImageConverter } from "./converters/image.js";
import { IpynbConverter as TsIpynbConverter } from "./converters/ipynb.js";
import { IWorkConverter as TsIWorkConverter } from "./converters/iwork.js";
import { JsonConverter as TsJsonConverter } from "./converters/json.js";
import { PdfConverter as TsPdfConverter } from "./converters/pdf/index.js";
import { PlainTextConverter as TsPlainTextConverter } from "./converters/plain-text.js";
import { PptxConverter as TsPptxConverter } from "./converters/pptx.js";
import { RssConverter as TsRssConverter } from "./converters/rss.js";
import { WikipediaConverter as TsWikipediaConverter } from "./converters/wikipedia.js";
import { XlsxConverter as TsXlsxConverter } from "./converters/xlsx.js";
import { XmlConverter as TsXmlConverter } from "./converters/xml.js";
import { YamlConverter as TsYamlConverter } from "./converters/yaml.js";
import { ZipConverter as TsZipConverter } from "./converters/zip.js";
import { Markit as TsMarkit } from "./markit.js";
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

// ---------- Markit class ----------

/**
 * Drop-in replacement for the TS Markit class.
 * Uses the native addon when available, falls back to TS.
 */
export class Markit {
  private _native: NativeMarkitInstance | null = null;
  private _ts: TsMarkit | null = null;

  constructor() {
    if (native) {
      this._native = new native.Markit();
    } else {
      this._ts = new TsMarkit();
    }
  }

  async convert(
    input: Buffer,
    streamInfo: StreamInfo,
  ): Promise<ConversionResult> {
    if (this._native) {
      return this._native.convert(input, streamInfo);
    }
    return (this._ts as TsMarkit).convert(input, streamInfo);
  }

  async convertFile(
    path: string,
    extra?: Partial<StreamInfo>,
  ): Promise<ConversionResult> {
    if (this._native) {
      return this._native.convertFile(path, extra);
    }
    return (this._ts as TsMarkit).convertFile(path, extra);
  }

  async convertUrl(url: string): Promise<ConversionResult> {
    if (this._native) {
      return this._native.convertUrl(url);
    }
    return (this._ts as TsMarkit).convertUrl(url);
  }
}

// ---------- Converter wrappers ----------

/**
 * Build a converter class with the same shape as the TS original:
 * native-by-name when the addon is loaded, TS instance otherwise.
 */
function makeConverterClass(converterName: string, tsFallback: Converter) {
  return class implements Converter {
    name = converterName;

    accepts(streamInfo: StreamInfo): boolean {
      if (native) {
        return native.converterAccepts(converterName, streamInfo);
      }
      return tsFallback.accepts(streamInfo);
    }

    async convert(
      input: Buffer,
      streamInfo: StreamInfo,
    ): Promise<ConversionResult> {
      if (native) {
        return native.converterConvert(converterName, input, streamInfo);
      }
      return tsFallback.convert(input, streamInfo);
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
      if (!tsFallback.convertUrl) {
        throw new Error(
          `Converter '${converterName}' does not support URL conversion`,
        );
      }
      return tsFallback.convertUrl(url);
    }
  };
}

export const AudioConverter = makeConverterClass(
  "audio",
  new TsAudioConverter(),
);
export const CsvConverter = makeConverterClass("csv", new TsCsvConverter());
export const DocxConverter = makeConverterClass("docx", new TsDocxConverter());
export const EpubConverter = makeConverterClass("epub", new TsEpubConverter());
export const GitHubConverter = makeConverterClass(
  "github",
  new TsGitHubConverter(),
);
export const HtmlConverter = makeConverterClass("html", new TsHtmlConverter());
export const ImageConverter = makeConverterClass(
  "image",
  new TsImageConverter(),
);
export const IpynbConverter = makeConverterClass(
  "ipynb",
  new TsIpynbConverter(),
);
export const IWorkConverter = makeConverterClass(
  "iwork",
  new TsIWorkConverter(),
);
export const JsonConverter = makeConverterClass("json", new TsJsonConverter());
export const PdfConverter = makeConverterClass("pdf", new TsPdfConverter());
export const PlainTextConverter = makeConverterClass(
  "plain-text",
  new TsPlainTextConverter(),
);
export const PptxConverter = makeConverterClass("pptx", new TsPptxConverter());
export const RssConverter = makeConverterClass("rss", new TsRssConverter());
export const WikipediaConverter = makeConverterClass(
  "wikipedia",
  new TsWikipediaConverter(),
);
export const XlsxConverter = makeConverterClass("xlsx", new TsXlsxConverter());
export const XmlConverter = makeConverterClass("xml", new TsXmlConverter());
export const YamlConverter = makeConverterClass("yaml", new TsYamlConverter());

/**
 * ZipConverter keeps the original constructor signature
 * `new ZipConverter(parentConverters)`. When a custom parent list is
 * supplied, conversion always uses the TS implementation (the native
 * registry only knows the builtins); with no argument, the native path
 * uses the builtin parent set.
 */
export class ZipConverter implements Converter {
  name = "zip";
  private readonly ts: TsZipConverter;
  private readonly custom: boolean;

  constructor(parentConverters: Converter[] = []) {
    this.custom = parentConverters.length > 0;
    this.ts = new TsZipConverter(parentConverters);
  }

  accepts(streamInfo: StreamInfo): boolean {
    if (native && !this.custom) {
      return native.converterAccepts("zip", streamInfo);
    }
    return this.ts.accepts(streamInfo);
  }

  async convert(
    input: Buffer,
    streamInfo: StreamInfo,
  ): Promise<ConversionResult> {
    if (native && !this.custom) {
      return native.converterConvert("zip", input, streamInfo);
    }
    return this.ts.convert(input, streamInfo);
  }
}
