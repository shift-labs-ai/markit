import type { ConversionResult, Converter, StreamInfo } from "../types.js";

const EXTENSIONS = [".vtt"];
const MIMETYPES = ["text/vtt", "text/webvtt"];
const SKIPPED_BLOCK_PREFIXES = ["WEBVTT", "NOTE", "STYLE", "REGION"];
const TIMESTAMP_TAG = /<\d{2}:\d{2}(?::\d{2})?\.\d{3}>/g;

const HTML_ENTITIES: Record<string, string> = {
  amp: "&",
  lt: "<",
  gt: ">",
  quot: '"',
  apos: "'",
  "#39": "'",
  "#x27": "'",
};

interface Cue {
  start: string;
  text: string;
}

export class VttConverter implements Converter {
  name = "vtt";

  accepts(streamInfo: StreamInfo): boolean {
    const extension = streamInfo.extension?.toLowerCase();
    const mimetype = streamInfo.mimetype?.toLowerCase();

    if (extension && EXTENSIONS.includes(extension)) {
      return true;
    }
    if (mimetype && MIMETYPES.some((m) => mimetype.startsWith(m))) {
      return true;
    }
    return false;
  }

  async convert(
    input: Buffer,
    streamInfo: StreamInfo,
  ): Promise<ConversionResult> {
    const text = new TextDecoder(streamInfo.charset || "utf-8").decode(input);
    const cues = parseVtt(text);

    if (cues.length === 0) {
      return { markdown: "" };
    }

    const transcript = buildIncrementalTranscript(cues);
    const lines = ["# Transcript", ""];

    if (transcript.cleanText) {
      lines.push("## Text", "", ...paragraphize(transcript.cleanText), "");
    }

    lines.push("## Timestamped Transcript", "");
    for (const cue of transcript.cues) {
      lines.push(`- [${cue.start}] ${cue.text}`);
    }

    return { markdown: lines.join("\n") };
  }
}

function parseVtt(input: string): Cue[] {
  const normalized = input.replace(/^\uFEFF/, "").replace(/\r\n?/g, "\n");
  const blocks = normalized.split(/\n{2,}/);
  const cues: Cue[] = [];

  for (const block of blocks) {
    const lines = block
      .split("\n")
      .map((line) => line.trim())
      .filter(Boolean);

    if (lines.length === 0) continue;
    if (SKIPPED_BLOCK_PREFIXES.some((prefix) => lines[0].startsWith(prefix))) {
      continue;
    }

    const timingIndex = lines.findIndex((line) => line.includes("-->"));
    if (timingIndex === -1) continue;

    const start = lines[timingIndex].split("-->")[0]?.trim();
    const cueText = cleanCueText(lines.slice(timingIndex + 1).join(" "));

    if (start && cueText) {
      cues.push({ start, text: cueText });
    }
  }

  return cues;
}

function cleanCueText(input: string): string {
  return input
    .replace(TIMESTAMP_TAG, "")
    .replace(/<[^>]+>/g, "")
    .replace(/&(#x?[0-9a-f]+|[a-z]+);/gi, (entity, name: string) =>
      decodeHtmlEntity(entity, name),
    )
    .replace(/\s+/g, " ")
    .trim();
}

function decodeHtmlEntity(entity: string, name: string): string {
  const normalized = name.toLowerCase();
  const named = HTML_ENTITIES[normalized];
  if (named) return named;

  if (normalized.startsWith("#x")) {
    return decodeCodePoint(entity, Number.parseInt(normalized.slice(2), 16));
  }
  if (normalized.startsWith("#")) {
    return decodeCodePoint(entity, Number.parseInt(normalized.slice(1), 10));
  }

  return entity;
}

function decodeCodePoint(fallback: string, codePoint: number): string {
  if (!Number.isFinite(codePoint)) return fallback;
  try {
    return String.fromCodePoint(codePoint);
  } catch {
    return fallback;
  }
}

function buildIncrementalTranscript(cues: Cue[]): {
  cleanText: string;
  cues: Cue[];
} {
  const words: string[] = [];
  const incrementalCues: Cue[] = [];
  let previousCueWords: string[] = [];

  for (const cue of cues) {
    const currentWords = cue.text.split(/\s+/).filter(Boolean);
    if (currentWords.length === 0) continue;

    if (
      currentWords.length <= previousCueWords.length &&
      (startsWithWords(previousCueWords, currentWords) ||
        endsWithWords(previousCueWords, currentWords))
    ) {
      previousCueWords = currentWords;
      continue;
    }

    let addedWords = currentWords;

    if (
      previousCueWords.length > 0 &&
      currentWords.length >= previousCueWords.length &&
      startsWithWords(currentWords, previousCueWords)
    ) {
      addedWords = currentWords.slice(previousCueWords.length);
    } else {
      const previousOverlap = suffixPrefixOverlap(
        previousCueWords,
        currentWords,
        30,
      );
      if (previousOverlap > 0) {
        addedWords = currentWords.slice(previousOverlap);
      } else {
        const transcriptOverlap = suffixPrefixOverlap(words, currentWords, 60);
        if (transcriptOverlap > 0) {
          addedWords = currentWords.slice(transcriptOverlap);
        }
      }
    }

    if (addedWords.length > 0) {
      const text = addedWords.join(" ");
      incrementalCues.push({ start: cue.start, text });
      words.push(...addedWords);
    }

    previousCueWords = currentWords;
  }

  return {
    cleanText: words.join(" "),
    cues: incrementalCues,
  };
}

function startsWithWords(words: string[], prefix: string[]): boolean {
  return prefix.every((word, index) => words[index] === word);
}

function endsWithWords(words: string[], suffix: string[]): boolean {
  return suffix.every(
    (word, index) => words[words.length - suffix.length + index] === word,
  );
}

function suffixPrefixOverlap(
  left: string[],
  right: string[],
  maxLength: number,
): number {
  let best = 0;
  const max = Math.min(left.length, right.length, maxLength);
  for (let length = 1; length <= max; length += 1) {
    if (endsWithWords(left, right.slice(0, length))) {
      best = length;
    }
  }
  return best;
}

function paragraphize(text: string): string[] {
  const sentences = text.split(/(?<=[.!?])\s+/);
  const paragraphs: string[] = [];
  let current: string[] = [];

  for (const sentence of sentences) {
    if (sentence) current.push(sentence);
    if (current.join(" ").length >= 700) {
      paragraphs.push(current.join(" "));
      current = [];
    }
  }

  if (current.length > 0) paragraphs.push(current.join(" "));
  return paragraphs;
}
