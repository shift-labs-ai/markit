/** PDF conversion is provided by Markit's MIT-licensed native Rust engine. */

import type { ConversionResult, Converter, StreamInfo } from "../../types.js";

const EXTENSIONS = [".pdf"];
const MIMETYPES = ["application/pdf", "application/x-pdf"];

export class PdfConverter implements Converter {
  name = "pdf";

  accepts(streamInfo: StreamInfo): boolean {
    return (
      (streamInfo.extension !== undefined &&
        EXTENSIONS.includes(streamInfo.extension)) ||
      (streamInfo.mimetype !== undefined &&
        MIMETYPES.some((mime) => streamInfo.mimetype?.startsWith(mime)))
    );
  }

  async convert(
    _input: Buffer,
    _streamInfo: StreamInfo,
  ): Promise<ConversionResult> {
    throw new Error(
      "PDF conversion requires a supported Markit native binary; the TypeScript fallback does not bundle a PDF engine",
    );
  }
}
