# markit

Convert anything to markdown. PDF, DOCX, PPTX, XLSX, HTML, EPUB, Jupyter, RSS, images, audio, URLs, and more. Pluggable converters, built-in LLM providers for image description and audio transcription. Works as a CLI and as a library.

```bash
npm install -g markit-ai
```

---

## Quick Start

```bash
# Documents
markit report.pdf
markit document.docx
markit slides.pptx

# Data
markit data.csv
markit config.json
markit schema.yaml

# Web
markit https://example.com/article
markit https://en.wikipedia.org/wiki/Markdown

# Media (via LLMs. set OPENAI_API_KEY or ANTHROPIC_API_KEY)
markit photo.jpg                          # EXIF metadata + AI description
markit recording.mp3                      # Audio metadata + transcription
markit photo.jpg -p "Extract all text"    # Custom instructions

# Write to file
markit report.pdf -o report.md

# Pipe it
markit report.pdf | pbcopy
markit data.xlsx -q | napkin create "Imported Data"
```

---

## Supported Formats

| Format | Extensions | How |
|--------|-----------|-----|
| PDF | `.pdf` | Text extraction via unpdf |
| Word | `.docx` | mammoth → turndown, preserves headings/tables |
| PowerPoint | `.pptx` | XML parsing, slides + notes + tables |
| Excel | `.xlsx` | Each sheet → markdown table |
| HTML | `.html` `.htm` | turndown, scripts/styles stripped |
| EPUB | `.epub` | Spine-ordered chapters, metadata header |
| Jupyter | `.ipynb` | Markdown cells + code + outputs |
| RSS/Atom | `.rss` `.atom` `.xml` | Feed items with dates and content |
| CSV/TSV | `.csv` `.tsv` | Markdown tables |
| JSON | `.json` | Pretty-printed code block |
| YAML | `.yaml` `.yml` | Code block |
| XML/SVG | `.xml` `.svg` | Code block |
| Images | `.jpg` `.png` `.gif` `.webp` | EXIF metadata + optional AI description |
| Audio | `.mp3` `.wav` `.m4a` `.flac` | Metadata + optional AI transcription |
| ZIP | `.zip` | Recursive. converts each file inside |
| URLs | `http://` `https://` | Fetches with `Accept: text/markdown` |
| Wikipedia | `*.wikipedia.org` | Main content extraction |
| Code | `.py` `.ts` `.go` `.rs` ... | Fenced code block |
| Plain text | `.txt` `.md` `.rst` `.log` | Pass-through |


---

## AI Features

Images and audio get metadata extraction for free. For AI-powered descriptions and transcription, set an API key:

```bash
# OpenAI (default provider)
export OPENAI_API_KEY=sk-...
markit photo.jpg

# Anthropic
markit config set llm.provider anthropic
export ANTHROPIC_API_KEY=sk-ant-...
markit photo.jpg

# Any OpenAI-compatible API (Ollama, Groq, Together, etc.)
markit config set llm.apiBase http://localhost:11434/v1
```

Focus the AI on what matters:

```bash
markit receipt.jpg -p "List all line items with prices as a table"
markit diagram.png -p "Describe the architecture and data flow"
markit whiteboard.jpg -p "Extract all text verbatim"
```

---

## For Agents

Every command supports `--json`. Raw markdown with `-q`.

```bash
markit report.pdf --json       # Structured output for parsing
markit report.pdf -q           # Raw markdown, nothing else
markit onboard                 # Add instructions to CLAUDE.md
```

---

## SDK

markit is also a library:

```typescript
import { Markit } from "markit-ai";

const markit = new Markit();
const { markdown } = await markit.convertFile("report.pdf");
const { markdown } = await markit.convertUrl("https://example.com");
const { markdown } = await markit.convert(buffer, { extension: ".docx" });
```

With AI features. pass plain functions, use any provider:

```typescript
import OpenAI from "openai";
import { Markit } from "markit-ai";

const openai = new OpenAI();

const markit = new Markit({
  describe: async (image, mime) => {
    const res = await openai.chat.completions.create({
      model: "gpt-4.1-nano",
      messages: [{ role: "user", content: [
        { type: "text", text: "Describe this image." },
        { type: "image_url", image_url: { url: `data:${mime};base64,${image.toString("base64")}` } },
      ]}],
    });
    return res.choices[0].message.content ?? "";
  },
  transcribe: async (audio, mime) => {
    const res = await openai.audio.transcriptions.create({
      model: "gpt-4o-mini-transcribe",
      file: new File([audio], "audio.mp3", { type: mime }),
    });
    return res.text;
  },
});
```

Mix providers. Claude for vision, OpenAI for audio, whatever:

```typescript
const markit = new Markit({
  describe: async (image, mime) => {
    const res = await anthropic.messages.create({
      model: "claude-haiku-4-5",
      messages: [{ role: "user", content: [
        { type: "image", source: { type: "base64", media_type: mime, data: image.toString("base64") } },
        { type: "text", text: "Describe this image." },
      ]}],
    });
    return res.content[0].text;
  },
  transcribe: async (audio, mime) => { /* Whisper, Deepgram, AssemblyAI, ... */ },
});
```

Or use the built-in providers. no SDK needed:

```typescript
import { Markit, createLlmFunctions, loadConfig } from "markit-ai";

const config = loadConfig(); // reads .markit/config.json + env vars
const markit = new Markit(createLlmFunctions(config));
```

---

## Configuration

```bash
markit init                              # Create .markit/config.json
markit config show                       # Show resolved settings
markit config get llm.model              # Get a value
markit config set llm.provider anthropic # Switch provider
markit config set llm.apiKey sk-...      # Set a value
```

`.markit/config.json`:

```json
{
  "llm": {
    "provider": "openai",
    "apiBase": "https://api.openai.com/v1",
    "apiKey": "sk-...",
    "model": "gpt-4.1-nano",
    "transcriptionModel": "gpt-4o-mini-transcribe"
  }
}
```

Env vars override config. Each provider checks its own env vars first:

| Provider | Env vars | Default model |
|----------|---------|---------------|
| `openai` | `OPENAI_API_KEY`, `MARKIT_API_KEY` | `gpt-4.1-nano` |
| `anthropic` | `ANTHROPIC_API_KEY`, `MARKIT_API_KEY` | `claude-haiku-4-5` |

---

## CLI Reference

```bash
markit <source>                          # Convert file or URL
markit <source> -o output.md             # Write to file
markit <source> -p "instructions"        # Custom AI prompt
markit <source> --json                   # JSON output
markit <source> -q                       # Raw markdown only
cat file.pdf | markit -                  # Read from stdin
markit formats                           # List supported formats
markit init                              # Create .markit/ config
markit config show                       # Show settings
markit config get <key>                  # Get config value
markit config set <key> <value>          # Set config value
markit onboard                           # Add to CLAUDE.md
```

---

## Development

```bash
bun install
bun run dev -- report.pdf
bun test
bun run check
```

## License

MIT
