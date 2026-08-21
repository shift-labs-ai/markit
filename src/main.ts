#!/usr/bin/env node

import { createRequire } from "node:module";
import { Command } from "commander";

// Command modules are imported lazily inside each action: the converter
// machinery costs ~100ms to load and --help/--version/startup should not
// pay for it (cf. anydoc's cli.js, which requires its binding only after
// argument parsing).

const require = createRequire(import.meta.url);
const { version } = require("../package.json");

const program = new Command();

program
  .name("markit")
  .description("Convert anything to markdown.")
  .version(`markit ${version}`, "-V, --version")
  .option("--json", "Output as JSON")
  .option("-q, --quiet", "Raw markdown only, no decoration")
  .option("-o, --output <file>", "Write to file instead of stdout")
  .option("-i, --image-dir <dir>", "Extract images to this directory")
  // Commander maps --no-images to opts.images === false.
  .option("--no-images", "Skip image extraction; images become comments")
  .option(
    "--page-markers",
    "Mark page boundaries with <!-- markit:page N --> (PDF only)",
  )
  .addHelpText(
    "after",
    `
Examples:
  $ markit report.pdf                  Convert a PDF to markdown
  $ markit document.docx -o doc.md     Convert DOCX, write to file
  $ markit https://example.com         Convert a web page
  $ markit photo.jpg                   Extract EXIF metadata
  $ markit recording.mp3               Extract audio metadata
  $ cat file.pdf | markit -            Read from stdin

Docs: https://github.com/shift-labs-ai/markit`,
  );

program
  .command("convert")
  .alias("c")
  .description("Convert a file or URL to markdown")
  .argument("<source>", "File path, URL, or - for stdin")
  .option("-o, --output <file>", "Write to file instead of stdout")
  .action(async (source, opts, cmd) => {
    const globals = cmd.optsWithGlobals();
    const { convert } = await import("./commands/convert.js");
    await convert(source, {
      json: globals.json,
      quiet: globals.quiet,
      output: opts.output,
      imageDir: globals.imageDir,
      noImages: globals.images === false,
      pageMarkers: globals.pageMarkers,
    });
  });

program
  .command("formats")
  .description("List supported formats")
  .action(async (_opts, cmd) => {
    const globals = cmd.optsWithGlobals();
    const { formats } = await import("./commands/formats.js");
    await formats([], { json: globals.json, quiet: globals.quiet });
  });

program
  .command("onboard")
  .description("Add markit instructions to CLAUDE.md or AGENTS.md")
  .action(async (_opts, cmd) => {
    const globals = cmd.optsWithGlobals();
    const { onboard } = await import("./commands/onboard.js");
    await onboard([], { json: globals.json, quiet: globals.quiet });
  });

// Default behavior: if first arg isn't a known subcommand, treat it as a source to convert
program.on("command:*", async (args) => {
  const source = args[0];
  if (!source) {
    program.help();
    return;
  }

  // Check for typos against known subcommands
  const commands = ["convert", "formats", "onboard", "help"];
  const close = commands.filter(
    (c) => levenshtein(source, c) <= 2 && source !== c,
  );
  if (
    close.length > 0 &&
    !source.includes("/") &&
    !source.includes(".") &&
    !source.startsWith("http")
  ) {
    const { error } = await import("./utils/output.js");
    error(`Unknown command '${source}'. Did you mean '${close[0]}'?`);
    process.exit(1);
  }

  const globals = program.opts();
  const { convert } = await import("./commands/convert.js");
  await convert(source, {
    json: globals.json,
    quiet: globals.quiet,
    output: globals.output,
    imageDir: globals.imageDir,
    noImages: globals.images === false,
    pageMarkers: globals.pageMarkers,
  });
});

// No args → show concise help
if (process.argv.length <= 2) {
  console.log(`markit — convert anything to markdown

Usage:  markit <file-or-url> [options]

Examples:
  $ markit report.pdf
  $ markit document.docx -o doc.md
  $ markit https://example.com

Commands:
  markit formats     List supported formats
  markit onboard     Add instructions to CLAUDE.md

Run markit --help for all options.
Docs: https://github.com/shift-labs-ai/markit`);
  process.exit(0);
}

program.parseAsync(process.argv).catch((err) => {
  console.error("Fatal error:", err.message);
  process.exit(1);
});

function levenshtein(a: string, b: string): number {
  const m = a.length;
  const n = b.length;
  const dp: number[][] = Array.from({ length: m + 1 }, () =>
    Array(n + 1).fill(0),
  );
  for (let i = 0; i <= m; i++) dp[i][0] = i;
  for (let j = 0; j <= n; j++) dp[0][j] = j;
  for (let i = 1; i <= m; i++) {
    for (let j = 1; j <= n; j++) {
      dp[i][j] = Math.min(
        dp[i - 1][j] + 1,
        dp[i][j - 1] + 1,
        dp[i - 1][j - 1] + (a[i - 1] !== b[j - 1] ? 1 : 0),
      );
    }
  }
  return dp[m][n];
}
