import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import type { MarkitPluginAPI } from "../../src/plugins/types.js";

export default function (api: MarkitPluginAPI) {
  api.setName("nutrient-pdf");
  api.setVersion("0.1.0");

  api.registerConverter(
    {
      name: "nutrient-pdf",

      accepts(streamInfo) {
        const ext = streamInfo.extension;
        const mime = streamInfo.mimetype;
        return (
          ext === ".pdf" ||
          mime === "application/pdf" ||
          mime === "application/x-pdf" ||
          false
        );
      },

      async convert(input) {
        // Write buffer to a temp file since the CLI reads from a path
        const tmp = mkdtempSync(join(tmpdir(), "nutrient-"));
        const tmpPdf = join(tmp, "input.pdf");
        try {
          writeFileSync(tmpPdf, input);
          const markdown = execFileSync("pdf-to-markdown", [tmpPdf], {
            encoding: "utf-8",
            maxBuffer: 100 * 1024 * 1024,
          });
          return { markdown };
        } finally {
          rmSync(tmp, { recursive: true, force: true });
        }
      },
    },
    { name: "PDF (Nutrient)", extensions: [".pdf"] },
  );
}
