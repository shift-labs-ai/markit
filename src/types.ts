export interface StreamInfo {
  mimetype?: string;
  extension?: string;
  charset?: string;
  filename?: string;
  localPath?: string;
  url?: string;
  /** Directory to write extracted images/diagrams. */
  imageDir?: string;
  /**
   * Emit `<!-- markit:page N -->` before each page's content, carrying
   * the 1-based physical page number. PDF only; off by default.
   */
  pageMarkers?: boolean;
}

export interface ConversionResult {
  markdown: string;
  title?: string;
}

export interface Converter {
  /** Human-readable name for error messages */
  name: string;

  /** Quick check: can this converter handle the given stream? */
  accepts(streamInfo: StreamInfo): boolean;

  /**
   * Optional URL-first hook. When present, called before the default fetch
   * so the converter can handle URL fetching itself (e.g. rewrite to a raw
   * content URL or call an API).
   */
  convertUrl?(url: string): Promise<ConversionResult>;

  /** Convert the source to markdown. */
  convert(input: Buffer, streamInfo?: StreamInfo): Promise<ConversionResult>;
}
