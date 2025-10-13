import { existsSync, mkdirSync, writeFileSync } from "fs";
import { join } from "path";
import { type Plugin } from "vite";

export default function ensureGitignore(): Plugin {
  let outDir: string;
  return {
    name: "ensure-gitignore",
    apply: "build",
    configResolved(config) {
      outDir = config.build.outDir;
    },
    buildStart: () => {
      // create the out dir if it doesn't exist
      if (!existsSync(outDir)) {
        mkdirSync(outDir, { recursive: true });
      }
      const ignored = join(outDir, ".gitignore");
      writeFileSync(ignored, "*\n");
      console.log(`[ensure-out-dir] ensured ${ignored}`);
    },
    closeBundle: () => {
      const gitignore = join(outDir, ".gitignore");
      if (!existsSync(gitignore)) {
        writeFileSync(gitignore, "*\n");
      }
    },
  };
}
