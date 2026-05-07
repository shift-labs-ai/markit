import { describe, expect, test } from "bun:test";
import { VttConverter } from "./vtt.js";

const converter = new VttConverter();

describe("VttConverter", () => {
  test("accepts .vtt files and WebVTT mimetypes", () => {
    expect(converter.accepts({ extension: ".vtt" })).toBe(true);
    expect(converter.accepts({ mimetype: "text/vtt; charset=utf-8" })).toBe(
      true,
    );
    expect(converter.accepts({ mimetype: "text/webvtt" })).toBe(true);
    expect(converter.accepts({ extension: ".txt" })).toBe(false);
  });

  test("converts simple WebVTT cues to markdown", async () => {
    const input = Buffer.from(`WEBVTT
Kind: captions
Language: en

00:00:00.000 --> 00:00:02.000
Hello world.

00:00:02.000 --> 00:00:04.000
This is a caption test.
`);

    const result = await converter.convert(input, { extension: ".vtt" });

    expect(result.markdown).toContain("# Transcript");
    expect(result.markdown).toContain("Hello world. This is a caption test.");
    expect(result.markdown).toContain("- [00:00:00.000] Hello world.");
    expect(result.markdown).toContain(
      "- [00:00:02.000] This is a caption test.",
    );
  });

  test("deduplicates YouTube rolling captions", async () => {
    const input = Buffer.from(`WEBVTT

00:00:00.400 --> 00:00:02.430 align:start position:0%
You're<00:00:00.560><c> lying</c><00:00:00.880><c> in</c><00:00:01.040><c> bed</c>

00:00:02.430 --> 00:00:02.440 align:start position:0%
You're lying in bed

00:00:02.440 --> 00:00:05.230 align:start position:0%
You're lying in bed
at<00:00:02.760><c> 2:00</c><00:00:02.960><c> a.m.</c>
`);

    const result = await converter.convert(input, { extension: ".vtt" });

    expect(result.markdown).toContain("You're lying in bed at 2:00 a.m.");
    expect(result.markdown.match(/You're lying in bed/g)?.length).toBe(2);
  });

  test("ignores comments and decodes entities", async () => {
    const input = Buffer.from(`WEBVTT

NOTE this should be ignored

00:00:00.000 --> 00:00:01.000
<v Speaker>Fish &amp; chips</v>
`);

    const result = await converter.convert(input, { extension: ".vtt" });

    expect(result.markdown).toContain("Fish & chips");
    expect(result.markdown).not.toContain("NOTE this should be ignored");
  });
});
