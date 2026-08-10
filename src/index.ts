// Public API — routes through native addon when available, TS fallback otherwise.
// Re-exports maintain the exact same surface as before.

export {
  AudioConverter,
  CsvConverter,
  DocxConverter,
  EpubConverter,
  GitHubConverter,
  HtmlConverter,
  ImageConverter,
  IpynbConverter,
  IWorkConverter,
  isNative,
  JsonConverter,
  Markit,
  PdfConverter,
  PlainTextConverter,
  PptxConverter,
  RssConverter,
  WikipediaConverter,
  XlsxConverter,
  XmlConverter,
  YamlConverter,
  ZipConverter,
} from "./native-markit.js";

export type {
  ConversionResult,
  Converter,
  StreamInfo,
} from "./types.js";
