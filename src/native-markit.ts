/**
 * Thin Node SDK over Markit's Rust engine.
 *
 * Conversion has one implementation: the native Rust addon. This file only
 * supplies stable TypeScript types and ergonomic converter-class wrappers.
 */

import { createRequire } from "node:module";
import type { ConversionResult, Converter, StreamInfo } from "./types.js";

export type { ConversionResult, Converter, StreamInfo } from "./types.js";

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

const require = createRequire(import.meta.url);
const native = require("../native.cjs") as NativeBinding;

/** Markit always runs through its native Rust engine. */
export const isNative = true;

/** Native Rust conversion registry. */
export class Markit {
  readonly #native = new native.Markit();

  convert(
    input: Buffer,
    streamInfo: StreamInfo = {},
  ): Promise<ConversionResult> {
    return this.#native.convert(input, streamInfo);
  }

  convertFile(
    path: string,
    extra?: Partial<StreamInfo>,
  ): Promise<ConversionResult> {
    return this.#native.convertFile(path, extra);
  }

  convertUrl(url: string): Promise<ConversionResult> {
    return this.#native.convertUrl(url);
  }
}

function makeConverterClass(converterName: string) {
  return class implements Converter {
    readonly name = converterName;

    accepts(streamInfo: StreamInfo): boolean {
      return native.converterAccepts(converterName, streamInfo);
    }

    convert(
      input: Buffer,
      streamInfo: StreamInfo = {},
    ): Promise<ConversionResult> {
      return native.converterConvert(converterName, input, streamInfo);
    }

    async convertUrl(url: string): Promise<ConversionResult> {
      const result = await native.converterConvertUrl(converterName, url);
      if (result === null) {
        throw new Error(
          `Converter '${converterName}' does not support URL conversion`,
        );
      }
      return result;
    }
  };
}

export const AudioConverter = makeConverterClass("audio");
export const CsvConverter = makeConverterClass("csv");
export const DocxConverter = makeConverterClass("docx");
export const EpubConverter = makeConverterClass("epub");
export const GitHubConverter = makeConverterClass("github");
export const HtmlConverter = makeConverterClass("html");
export const ImageConverter = makeConverterClass("image");
export const IpynbConverter = makeConverterClass("ipynb");
export const IWorkConverter = makeConverterClass("iwork");
export const JsonConverter = makeConverterClass("json");
export const PdfConverter = makeConverterClass("pdf");
export const PlainTextConverter = makeConverterClass("plain-text");
export const PptxConverter = makeConverterClass("pptx");
export const RssConverter = makeConverterClass("rss");
export const WikipediaConverter = makeConverterClass("wikipedia");
export const XlsxConverter = makeConverterClass("xlsx");
export const XmlConverter = makeConverterClass("xml");
export const YamlConverter = makeConverterClass("yaml");

/** ZIP conversion uses the Rust registry's builtin recursive converters. */
export const ZipConverter = makeConverterClass("zip");
