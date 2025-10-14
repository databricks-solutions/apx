// src/apx/plugins/index.ts
import { existsSync, mkdirSync, writeFileSync } from "fs";
import { join, resolve } from "path";
import { exec } from "child_process";
import { promisify } from "util";
import { generate } from "orval";
function apx(options = {}) {
  const { steps = [], ignore = [] } = options;
  let outDir;
  let timer = null;
  let stopping = false;
  let resolvedIgnores = [];
  let isRunningSteps = false;
  async function executeAction(action) {
    if (stopping) {
      console.log(`[apx] skipping action (stopping): ${action}`);
      return;
    }
    console.log(`[apx] executing action: ${action}`);
    ensureOutDirAndGitignore();
    if (typeof action === "string") {
      const { stdout, stderr } = await execAsync(action);
      if (stdout)
        console.log(stdout.trim());
      if (stderr)
        console.error(stderr.trim());
    } else {
      await action();
    }
    ensureOutDirAndGitignore();
  }
  async function runAllSteps() {
    if (stopping) {
      console.log(`[apx] skipping steps (stopping)`);
      return;
    }
    if (isRunningSteps) {
      console.log(`[apx] steps already running, skipping`);
      return;
    }
    isRunningSteps = true;
    console.log(`[apx] starting ${steps.length} step(s)`);
    try {
      for (const step of steps) {
        if (stopping) {
          console.log(`[apx] stopping steps early`);
          break;
        }
        const start = Date.now();
        try {
          console.log(`[apx] ${step.name} \u23F3`);
          await executeAction(step.action);
          console.log(`[apx] ${step.name} \u2713 (${Date.now() - start} ms)`);
        } catch (err) {
          console.error(`[apx] ${step.name} \u2717`, err);
          throw err;
        }
      }
    } finally {
      isRunningSteps = false;
      console.log(`[apx] finished running steps`);
    }
  }
  function ensureOutDirAndGitignore() {
    if (!outDir) {
      console.error(`[apx] outDir is not set`);
      return;
    }
    try {
      if (!existsSync(outDir)) {
        mkdirSync(outDir, { recursive: true });
      }
      const gitignorePath = join(outDir, ".gitignore");
      if (!existsSync(gitignorePath)) {
        writeFileSync(gitignorePath, "*\n");
      }
    } catch (err) {
      console.error(`[apx] failed to ensure output directory:`, err);
    }
  }
  function stop() {
    if (stopping) {
      console.log("[apx] already stopping, ignoring");
      return;
    }
    console.log("[apx] stop() called");
    stopping = true;
    if (timer) {
      console.log("[apx] clearing pending timer");
      clearTimeout(timer);
      timer = null;
    }
    console.log("[apx] stopped");
  }
  function reset() {
    console.log("[apx] reset() called");
    stopping = false;
    timer = null;
    isRunningSteps = false;
  }
  return {
    name: "apx",
    apply: () => true,
    configResolved(config) {
      console.log("[apx] configResolved() called");
      outDir = resolve(config.root, config.build.outDir);
      console.log(`[apx] outDir resolved to: ${outDir}`);
      resolvedIgnores = ignore.map((pattern) => resolve(process.cwd(), pattern));
      reset();
      ensureOutDirAndGitignore();
    },
    configureServer(server) {
      console.log("[apx] configureServer() called");
      server.httpServer?.once("close", () => {
        console.log("[apx] server.httpServer 'close' event fired");
        stop();
      });
      ensureOutDirAndGitignore();
    },
    async buildStart() {
      console.log("[apx] buildStart() called");
      ensureOutDirAndGitignore();
      if (steps.length > 0) {
        await runAllSteps();
      }
    },
    handleHotUpdate(ctx) {
      console.log(`[apx] handleHotUpdate() called for: ${ctx.file}`);
      ensureOutDirAndGitignore();
      if (resolvedIgnores.some((pattern) => ctx.file.includes(pattern))) {
        console.log(`[apx] file ignored: ${ctx.file}`);
        return;
      }
      if (timer) {
        console.log("[apx] clearing existing timer");
        clearTimeout(timer);
      }
      console.log("[apx] setting timer for step execution");
      timer = setTimeout(async () => {
        console.log("[apx] timer fired, running steps");
        timer = null;
        ensureOutDirAndGitignore();
        await runAllSteps();
        ensureOutDirAndGitignore();
      }, 100);
      timer.unref();
    },
    writeBundle() {
      console.log("[apx] writeBundle() called");
      ensureOutDirAndGitignore();
    },
    closeBundle() {
      console.log("[apx] closeBundle() called");
      ensureOutDirAndGitignore();
      stop();
    }
  };
}
var execAsync = promisify(exec);
var Step = (spec) => spec;
var OpenAPI = (appModule, outputPath) => ({
  name: "openapi",
  action: `uv run apx openapi ${appModule} ${outputPath}`
});
var Orval = (config) => ({
  name: "orval",
  action: () => generate(config)
});
var plugins_default = apx;
export {
  plugins_default as default,
  apx,
  Step,
  Orval,
  OpenAPI
};

//# debugId=473382D35B2BBEFC64756E2164756E21
//# sourceMappingURL=data:application/json;base64,ewogICJ2ZXJzaW9uIjogMywKICAic291cmNlcyI6IFsic3JjL2FweC9wbHVnaW5zL2luZGV4LnRzIl0sCiAgInNvdXJjZXNDb250ZW50IjogWwogICAgImltcG9ydCB7IGV4aXN0c1N5bmMsIG1rZGlyU3luYywgd3JpdGVGaWxlU3luYyB9IGZyb20gXCJmc1wiO1xuaW1wb3J0IHsgam9pbiwgcmVzb2x2ZSB9IGZyb20gXCJwYXRoXCI7XG5pbXBvcnQgeyB0eXBlIFBsdWdpbiB9IGZyb20gXCJ2aXRlXCI7XG5pbXBvcnQgeyBleGVjIH0gZnJvbSBcImNoaWxkX3Byb2Nlc3NcIjtcbmltcG9ydCB7IHByb21pc2lmeSB9IGZyb20gXCJ1dGlsXCI7XG5pbXBvcnQgeyBnZW5lcmF0ZSwgdHlwZSBPcHRpb25zRXhwb3J0IGFzIE9ydmFsQ29uZmlnIH0gZnJvbSBcIm9ydmFsXCI7XG5cbmNvbnN0IGV4ZWNBc3luYyA9IHByb21pc2lmeShleGVjKTtcblxuLy8gUmUtZXhwb3J0IE9ydmFsQ29uZmlnIGZvciBjb252ZW5pZW5jZVxuZXhwb3J0IHR5cGUgeyBPcnZhbENvbmZpZyB9O1xuXG5leHBvcnQgdHlwZSBTdGVwQWN0aW9uID0gc3RyaW5nIHwgKCgpID0+IHZvaWQgfCBQcm9taXNlPHZvaWQ+KTtcblxuZXhwb3J0IHR5cGUgU3RlcFNwZWMgPSB7XG4gIG5hbWU6IHN0cmluZztcbiAgYWN0aW9uOiBTdGVwQWN0aW9uO1xufTtcblxuZXhwb3J0IGNvbnN0IFN0ZXAgPSAoc3BlYzogU3RlcFNwZWMpOiBTdGVwU3BlYyA9PiBzcGVjO1xuXG4vKipcbiAqIFByZWRlZmluZWQgc3RlcCBmb3IgZ2VuZXJhdGluZyBPcGVuQVBJIHNjaGVtYVxuICogQHBhcmFtIGFwcE1vZHVsZSAtIFRoZSBQeXRob24gbW9kdWxlIHBhdGggKGUuZy4sIFwic2FtcGxlLmFwaS5hcHA6YXBwXCIpXG4gKiBAcGFyYW0gb3V0cHV0UGF0aCAtIFdoZXJlIHRvIHdyaXRlIHRoZSBPcGVuQVBJIEpTT04gZmlsZVxuICovXG5leHBvcnQgY29uc3QgT3BlbkFQSSA9IChhcHBNb2R1bGU6IHN0cmluZywgb3V0cHV0UGF0aDogc3RyaW5nKTogU3RlcFNwZWMgPT4gKHtcbiAgbmFtZTogXCJvcGVuYXBpXCIsXG4gIGFjdGlvbjogYHV2IHJ1biBhcHggb3BlbmFwaSAke2FwcE1vZHVsZX0gJHtvdXRwdXRQYXRofWAsXG59KTtcblxuLyoqXG4gKiBQcmVkZWZpbmVkIHN0ZXAgZm9yIGdlbmVyYXRpbmcgQVBJIGNsaWVudCB3aXRoIE9ydmFsXG4gKiBAcGFyYW0gY29uZmlnIC0gT3J2YWwgY29uZmlndXJhdGlvbiBvYmplY3RcbiAqL1xuZXhwb3J0IGNvbnN0IE9ydmFsID0gKGNvbmZpZzogT3J2YWxDb25maWcpOiBTdGVwU3BlYyA9PiAoe1xuICBuYW1lOiBcIm9ydmFsXCIsXG4gIGFjdGlvbjogKCkgPT4gZ2VuZXJhdGUoY29uZmlnKSxcbn0pO1xuZXhwb3J0IGludGVyZmFjZSBBcHhQbHVnaW5PcHRpb25zIHtcbiAgc3RlcHM/OiBTdGVwU3BlY1tdO1xuICBpZ25vcmU/OiBzdHJpbmdbXTtcbn1cblxuZXhwb3J0IGZ1bmN0aW9uIGFweChvcHRpb25zOiBBcHhQbHVnaW5PcHRpb25zID0ge30pOiBQbHVnaW4ge1xuICBjb25zdCB7IHN0ZXBzID0gW10sIGlnbm9yZSA9IFtdIH0gPSBvcHRpb25zO1xuXG4gIGxldCBvdXREaXI6IHN0cmluZztcbiAgbGV0IHRpbWVyOiBOb2RlSlMuVGltZW91dCB8IG51bGwgPSBudWxsO1xuICBsZXQgc3RvcHBpbmcgPSBmYWxzZTtcbiAgbGV0IHJlc29sdmVkSWdub3Jlczogc3RyaW5nW10gPSBbXTtcbiAgbGV0IGlzUnVubmluZ1N0ZXBzID0gZmFsc2U7XG5cbiAgYXN5bmMgZnVuY3Rpb24gZXhlY3V0ZUFjdGlvbihhY3Rpb246IFN0ZXBBY3Rpb24pOiBQcm9taXNlPHZvaWQ+IHtcbiAgICBpZiAoc3RvcHBpbmcpIHtcbiAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSBza2lwcGluZyBhY3Rpb24gKHN0b3BwaW5nKTogJHthY3Rpb259YCk7XG4gICAgICByZXR1cm47XG4gICAgfVxuICAgIFxuICAgIGNvbnNvbGUubG9nKGBbYXB4XSBleGVjdXRpbmcgYWN0aW9uOiAke2FjdGlvbn1gKTtcbiAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcbiAgICBpZiAodHlwZW9mIGFjdGlvbiA9PT0gXCJzdHJpbmdcIikge1xuICAgICAgLy8gRXhlY3V0ZSBhcyBzaGVsbCBjb21tYW5kXG4gICAgICBjb25zdCB7IHN0ZG91dCwgc3RkZXJyIH0gPSBhd2FpdCBleGVjQXN5bmMoYWN0aW9uKTtcbiAgICAgIGlmIChzdGRvdXQpIGNvbnNvbGUubG9nKHN0ZG91dC50cmltKCkpO1xuICAgICAgaWYgKHN0ZGVycikgY29uc29sZS5lcnJvcihzdGRlcnIudHJpbSgpKTtcbiAgICB9IGVsc2Uge1xuICAgICAgLy8gRXhlY3V0ZSBhcyBmdW5jdGlvblxuICAgICAgYXdhaXQgYWN0aW9uKCk7XG4gICAgfVxuICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICB9XG5cbiAgYXN5bmMgZnVuY3Rpb24gcnVuQWxsU3RlcHMoKTogUHJvbWlzZTx2b2lkPiB7XG4gICAgaWYgKHN0b3BwaW5nKSB7XG4gICAgICBjb25zb2xlLmxvZyhgW2FweF0gc2tpcHBpbmcgc3RlcHMgKHN0b3BwaW5nKWApO1xuICAgICAgcmV0dXJuO1xuICAgIH1cbiAgICBcbiAgICBpZiAoaXNSdW5uaW5nU3RlcHMpIHtcbiAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSBzdGVwcyBhbHJlYWR5IHJ1bm5pbmcsIHNraXBwaW5nYCk7XG4gICAgICByZXR1cm47XG4gICAgfVxuICAgIFxuICAgIGlzUnVubmluZ1N0ZXBzID0gdHJ1ZTtcbiAgICBjb25zb2xlLmxvZyhgW2FweF0gc3RhcnRpbmcgJHtzdGVwcy5sZW5ndGh9IHN0ZXAocylgKTtcbiAgICBcbiAgICB0cnkge1xuICAgICAgZm9yIChjb25zdCBzdGVwIG9mIHN0ZXBzKSB7XG4gICAgICAgIGlmIChzdG9wcGluZykge1xuICAgICAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSBzdG9wcGluZyBzdGVwcyBlYXJseWApO1xuICAgICAgICAgIGJyZWFrO1xuICAgICAgICB9XG4gICAgICAgIGNvbnN0IHN0YXJ0ID0gRGF0ZS5ub3coKTtcbiAgICAgICAgdHJ5IHtcbiAgICAgICAgICBjb25zb2xlLmxvZyhgW2FweF0gJHtzdGVwLm5hbWV9IOKPs2ApO1xuICAgICAgICAgIGF3YWl0IGV4ZWN1dGVBY3Rpb24oc3RlcC5hY3Rpb24pO1xuICAgICAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSAke3N0ZXAubmFtZX0g4pyTICgke0RhdGUubm93KCkgLSBzdGFydH0gbXMpYCk7XG4gICAgICAgIH0gY2F0Y2ggKGVycikge1xuICAgICAgICAgIGNvbnNvbGUuZXJyb3IoYFthcHhdICR7c3RlcC5uYW1lfSDinJdgLCBlcnIpO1xuICAgICAgICAgIHRocm93IGVycjtcbiAgICAgICAgfVxuICAgICAgfVxuICAgIH0gZmluYWxseSB7XG4gICAgICBpc1J1bm5pbmdTdGVwcyA9IGZhbHNlO1xuICAgICAgY29uc29sZS5sb2coYFthcHhdIGZpbmlzaGVkIHJ1bm5pbmcgc3RlcHNgKTtcbiAgICB9XG4gIH1cblxuICAvKipcbiAgICogRW5zdXJlcyB0aGUgb3V0cHV0IGRpcmVjdG9yeSBleGlzdHMgYW5kIGNvbnRhaW5zIGEgLmdpdGlnbm9yZSBmaWxlLlxuICAgKiBUaGlzIGlzIGNhbGxlZCBhdCBtdWx0aXBsZSBwb2ludHMgdG8gZ3VhcmFudGVlIHRoZSBkaXJlY3RvcnkgaXMgYWx3YXlzIHByZXNlbnQuXG4gICAqL1xuICBmdW5jdGlvbiBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTogdm9pZCB7XG4gICAgaWYgKCFvdXREaXIpIHtcbiAgICAgIGNvbnNvbGUuZXJyb3IoYFthcHhdIG91dERpciBpcyBub3Qgc2V0YCk7XG4gICAgICByZXR1cm47XG4gICAgfVxuXG4gICAgdHJ5IHtcbiAgICAgIC8vIEFsd2F5cyBlbnN1cmUgdGhlIG91dHB1dCBkaXJlY3RvcnkgZXhpc3RzXG4gICAgICBpZiAoIWV4aXN0c1N5bmMob3V0RGlyKSkge1xuICAgICAgICBta2RpclN5bmMob3V0RGlyLCB7IHJlY3Vyc2l2ZTogdHJ1ZSB9KTtcbiAgICAgIH1cblxuICAgICAgLy8gQWx3YXlzIGVuc3VyZSAuZ2l0aWdub3JlIGV4aXN0cyBpbiBvdXRwdXQgZGlyZWN0b3J5XG4gICAgICBjb25zdCBnaXRpZ25vcmVQYXRoID0gam9pbihvdXREaXIsIFwiLmdpdGlnbm9yZVwiKTtcbiAgICAgIGlmICghZXhpc3RzU3luYyhnaXRpZ25vcmVQYXRoKSkge1xuICAgICAgICB3cml0ZUZpbGVTeW5jKGdpdGlnbm9yZVBhdGgsIFwiKlxcblwiKTtcbiAgICAgIH1cbiAgICB9IGNhdGNoIChlcnIpIHtcbiAgICAgIGNvbnNvbGUuZXJyb3IoYFthcHhdIGZhaWxlZCB0byBlbnN1cmUgb3V0cHV0IGRpcmVjdG9yeTpgLCBlcnIpO1xuICAgIH1cbiAgfVxuXG4gIGZ1bmN0aW9uIHN0b3AoKTogdm9pZCB7XG4gICAgaWYgKHN0b3BwaW5nKSB7XG4gICAgICBjb25zb2xlLmxvZyhcIlthcHhdIGFscmVhZHkgc3RvcHBpbmcsIGlnbm9yaW5nXCIpO1xuICAgICAgcmV0dXJuO1xuICAgIH1cbiAgICBjb25zb2xlLmxvZyhcIlthcHhdIHN0b3AoKSBjYWxsZWRcIik7XG4gICAgc3RvcHBpbmcgPSB0cnVlO1xuICAgIGlmICh0aW1lcikge1xuICAgICAgY29uc29sZS5sb2coXCJbYXB4XSBjbGVhcmluZyBwZW5kaW5nIHRpbWVyXCIpO1xuICAgICAgY2xlYXJUaW1lb3V0KHRpbWVyKTtcbiAgICAgIHRpbWVyID0gbnVsbDtcbiAgICB9XG4gICAgY29uc29sZS5sb2coXCJbYXB4XSBzdG9wcGVkXCIpO1xuICB9XG5cbiAgZnVuY3Rpb24gcmVzZXQoKTogdm9pZCB7XG4gICAgY29uc29sZS5sb2coXCJbYXB4XSByZXNldCgpIGNhbGxlZFwiKTtcbiAgICBzdG9wcGluZyA9IGZhbHNlO1xuICAgIHRpbWVyID0gbnVsbDtcbiAgICBpc1J1bm5pbmdTdGVwcyA9IGZhbHNlO1xuICB9XG5cbiAgcmV0dXJuIHtcbiAgICBuYW1lOiBcImFweFwiLFxuICAgIGFwcGx5OiAoKSA9PiB0cnVlLFxuXG4gICAgY29uZmlnUmVzb2x2ZWQoY29uZmlnKSB7XG4gICAgICBjb25zb2xlLmxvZyhcIlthcHhdIGNvbmZpZ1Jlc29sdmVkKCkgY2FsbGVkXCIpO1xuICAgICAgb3V0RGlyID0gcmVzb2x2ZShjb25maWcucm9vdCwgY29uZmlnLmJ1aWxkLm91dERpcik7XG4gICAgICBjb25zb2xlLmxvZyhgW2FweF0gb3V0RGlyIHJlc29sdmVkIHRvOiAke291dERpcn1gKTtcbiAgICAgIHJlc29sdmVkSWdub3JlcyA9IGlnbm9yZS5tYXAoKHBhdHRlcm4pID0+XG4gICAgICAgIHJlc29sdmUocHJvY2Vzcy5jd2QoKSwgcGF0dGVybiksXG4gICAgICApO1xuXG4gICAgICAvLyBSZXNldCBzdGF0ZSBmb3IgbmV3IGJ1aWxkXG4gICAgICByZXNldCgpO1xuXG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBhcyBzb29uIGFzIHdlIGtub3cgdGhlIG91dERpclxuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgfSxcblxuICAgIGNvbmZpZ3VyZVNlcnZlcihzZXJ2ZXIpIHtcbiAgICAgIGNvbnNvbGUubG9nKFwiW2FweF0gY29uZmlndXJlU2VydmVyKCkgY2FsbGVkXCIpO1xuICAgICAgc2VydmVyLmh0dHBTZXJ2ZXI/Lm9uY2UoXCJjbG9zZVwiLCAoKSA9PiB7XG4gICAgICAgIGNvbnNvbGUubG9nKFwiW2FweF0gc2VydmVyLmh0dHBTZXJ2ZXIgJ2Nsb3NlJyBldmVudCBmaXJlZFwiKTtcbiAgICAgICAgc3RvcCgpO1xuICAgICAgfSk7XG5cbiAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIHdoZW4gc2VydmVyIHN0YXJ0c1xuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgfSxcblxuICAgIGFzeW5jIGJ1aWxkU3RhcnQoKSB7XG4gICAgICBjb25zb2xlLmxvZyhcIlthcHhdIGJ1aWxkU3RhcnQoKSBjYWxsZWRcIik7XG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBiZWZvcmUgYnVpbGQgc3RhcnRzXG4gICAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcblxuICAgICAgaWYgKHN0ZXBzLmxlbmd0aCA+IDApIHtcbiAgICAgICAgYXdhaXQgcnVuQWxsU3RlcHMoKTtcbiAgICAgIH1cbiAgICB9LFxuXG4gICAgaGFuZGxlSG90VXBkYXRlKGN0eCkge1xuICAgICAgY29uc29sZS5sb2coYFthcHhdIGhhbmRsZUhvdFVwZGF0ZSgpIGNhbGxlZCBmb3I6ICR7Y3R4LmZpbGV9YCk7XG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBvbiBldmVyeSBITVIgdXBkYXRlXG4gICAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcblxuICAgICAgLy8gQ2hlY2sgaWYgZmlsZSBzaG91bGQgYmUgaWdub3JlZFxuICAgICAgaWYgKHJlc29sdmVkSWdub3Jlcy5zb21lKChwYXR0ZXJuKSA9PiBjdHguZmlsZS5pbmNsdWRlcyhwYXR0ZXJuKSkpIHtcbiAgICAgICAgY29uc29sZS5sb2coYFthcHhdIGZpbGUgaWdub3JlZDogJHtjdHguZmlsZX1gKTtcbiAgICAgICAgcmV0dXJuO1xuICAgICAgfVxuXG4gICAgICAvLyBEZWJvdW5jZSBzdGVwIGV4ZWN1dGlvbiBvbiBITVIgdXBkYXRlc1xuICAgICAgaWYgKHRpbWVyKSB7XG4gICAgICAgIGNvbnNvbGUubG9nKFwiW2FweF0gY2xlYXJpbmcgZXhpc3RpbmcgdGltZXJcIik7XG4gICAgICAgIGNsZWFyVGltZW91dCh0aW1lcik7XG4gICAgICB9XG4gICAgICBjb25zb2xlLmxvZyhcIlthcHhdIHNldHRpbmcgdGltZXIgZm9yIHN0ZXAgZXhlY3V0aW9uXCIpO1xuICAgICAgdGltZXIgPSBzZXRUaW1lb3V0KGFzeW5jICgpID0+IHtcbiAgICAgICAgY29uc29sZS5sb2coXCJbYXB4XSB0aW1lciBmaXJlZCwgcnVubmluZyBzdGVwc1wiKTtcbiAgICAgICAgdGltZXIgPSBudWxsO1xuXG4gICAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIGJlZm9yZSBydW5uaW5nIHN0ZXBzXG4gICAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgICAgICBhd2FpdCBydW5BbGxTdGVwcygpO1xuXG4gICAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIGFmdGVyIHJ1bm5pbmcgc3RlcHNcbiAgICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgICB9LCAxMDApO1xuICAgICAgXG4gICAgICAvLyBBbGxvdyB0aGUgcHJvY2VzcyB0byBleGl0IGV2ZW4gaWYgdGhpcyB0aW1lciBpcyBwZW5kaW5nXG4gICAgICB0aW1lci51bnJlZigpO1xuICAgIH0sXG5cbiAgICB3cml0ZUJ1bmRsZSgpIHtcbiAgICAgIGNvbnNvbGUubG9nKFwiW2FweF0gd3JpdGVCdW5kbGUoKSBjYWxsZWRcIik7XG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBhZnRlciBmaWxlcyBhcmUgd3JpdHRlblxuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgfSxcblxuICAgIGNsb3NlQnVuZGxlKCkge1xuICAgICAgY29uc29sZS5sb2coXCJbYXB4XSBjbG9zZUJ1bmRsZSgpIGNhbGxlZFwiKTtcbiAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIG9uZSBmaW5hbCB0aW1lXG4gICAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcbiAgICAgIHN0b3AoKTtcbiAgICB9LFxuICB9O1xufVxuXG4vLyBEZWZhdWx0IGV4cG9ydCBmb3IgY29udmVuaWVuY2U6IGltcG9ydCBhcHggZnJvbSBcImFweFwiXG5leHBvcnQgZGVmYXVsdCBhcHg7XG4iCiAgXSwKICAibWFwcGluZ3MiOiAiO0FBQUE7QUFDQTtBQUVBO0FBQ0E7QUFDQTtBQXVDTyxTQUFTLEdBQUcsQ0FBQyxVQUE0QixDQUFDLEdBQVc7QUFDMUQsVUFBUSxRQUFRLENBQUMsR0FBRyxTQUFTLENBQUMsTUFBTTtBQUVwQyxNQUFJO0FBQ0osTUFBSSxRQUErQjtBQUNuQyxNQUFJLFdBQVc7QUFDZixNQUFJLGtCQUE0QixDQUFDO0FBQ2pDLE1BQUksaUJBQWlCO0FBRXJCLGlCQUFlLGFBQWEsQ0FBQyxRQUFtQztBQUM5RCxRQUFJLFVBQVU7QUFDWixjQUFRLElBQUkscUNBQXFDLFFBQVE7QUFDekQ7QUFBQSxJQUNGO0FBRUEsWUFBUSxJQUFJLDJCQUEyQixRQUFRO0FBQy9DLDZCQUF5QjtBQUN6QixlQUFXLFdBQVcsVUFBVTtBQUU5QixjQUFRLFFBQVEsV0FBVyxNQUFNLFVBQVUsTUFBTTtBQUNqRCxVQUFJO0FBQVEsZ0JBQVEsSUFBSSxPQUFPLEtBQUssQ0FBQztBQUNyQyxVQUFJO0FBQVEsZ0JBQVEsTUFBTSxPQUFPLEtBQUssQ0FBQztBQUFBLElBQ3pDLE9BQU87QUFFTCxZQUFNLE9BQU87QUFBQTtBQUVmLDZCQUF5QjtBQUFBO0FBRzNCLGlCQUFlLFdBQVcsR0FBa0I7QUFDMUMsUUFBSSxVQUFVO0FBQ1osY0FBUSxJQUFJLGlDQUFpQztBQUM3QztBQUFBLElBQ0Y7QUFFQSxRQUFJLGdCQUFnQjtBQUNsQixjQUFRLElBQUksdUNBQXVDO0FBQ25EO0FBQUEsSUFDRjtBQUVBLHFCQUFpQjtBQUNqQixZQUFRLElBQUksa0JBQWtCLE1BQU0sZ0JBQWdCO0FBRXBELFFBQUk7QUFDRixpQkFBVyxRQUFRLE9BQU87QUFDeEIsWUFBSSxVQUFVO0FBQ1osa0JBQVEsSUFBSSw0QkFBNEI7QUFDeEM7QUFBQSxRQUNGO0FBQ0EsY0FBTSxRQUFRLEtBQUssSUFBSTtBQUN2QixZQUFJO0FBQ0Ysa0JBQVEsSUFBSSxTQUFTLEtBQUssYUFBTztBQUNqQyxnQkFBTSxjQUFjLEtBQUssTUFBTTtBQUMvQixrQkFBUSxJQUFJLFNBQVMsS0FBSyxnQkFBVSxLQUFLLElBQUksSUFBSSxXQUFXO0FBQUEsaUJBQ3JELEtBQVA7QUFDQSxrQkFBUSxNQUFNLFNBQVMsS0FBSyxlQUFTLEdBQUc7QUFDeEMsZ0JBQU07QUFBQTtBQUFBLE1BRVY7QUFBQSxjQUNBO0FBQ0EsdUJBQWlCO0FBQ2pCLGNBQVEsSUFBSSw4QkFBOEI7QUFBQTtBQUFBO0FBUTlDLFdBQVMsd0JBQXdCLEdBQVM7QUFDeEMsU0FBSyxRQUFRO0FBQ1gsY0FBUSxNQUFNLHlCQUF5QjtBQUN2QztBQUFBLElBQ0Y7QUFFQSxRQUFJO0FBRUYsV0FBSyxXQUFXLE1BQU0sR0FBRztBQUN2QixrQkFBVSxRQUFRLEVBQUUsV0FBVyxLQUFLLENBQUM7QUFBQSxNQUN2QztBQUdBLFlBQU0sZ0JBQWdCLEtBQUssUUFBUSxZQUFZO0FBQy9DLFdBQUssV0FBVyxhQUFhLEdBQUc7QUFDOUIsc0JBQWMsZUFBZSxLQUFLO0FBQUEsTUFDcEM7QUFBQSxhQUNPLEtBQVA7QUFDQSxjQUFRLE1BQU0sNENBQTRDLEdBQUc7QUFBQTtBQUFBO0FBSWpFLFdBQVMsSUFBSSxHQUFTO0FBQ3BCLFFBQUksVUFBVTtBQUNaLGNBQVEsSUFBSSxrQ0FBa0M7QUFDOUM7QUFBQSxJQUNGO0FBQ0EsWUFBUSxJQUFJLHFCQUFxQjtBQUNqQyxlQUFXO0FBQ1gsUUFBSSxPQUFPO0FBQ1QsY0FBUSxJQUFJLDhCQUE4QjtBQUMxQyxtQkFBYSxLQUFLO0FBQ2xCLGNBQVE7QUFBQSxJQUNWO0FBQ0EsWUFBUSxJQUFJLGVBQWU7QUFBQTtBQUc3QixXQUFTLEtBQUssR0FBUztBQUNyQixZQUFRLElBQUksc0JBQXNCO0FBQ2xDLGVBQVc7QUFDWCxZQUFRO0FBQ1IscUJBQWlCO0FBQUE7QUFHbkIsU0FBTztBQUFBLElBQ0wsTUFBTTtBQUFBLElBQ04sT0FBTyxNQUFNO0FBQUEsSUFFYixjQUFjLENBQUMsUUFBUTtBQUNyQixjQUFRLElBQUksK0JBQStCO0FBQzNDLGVBQVMsUUFBUSxPQUFPLE1BQU0sT0FBTyxNQUFNLE1BQU07QUFDakQsY0FBUSxJQUFJLDZCQUE2QixRQUFRO0FBQ2pELHdCQUFrQixPQUFPLElBQUksQ0FBQyxZQUM1QixRQUFRLFFBQVEsSUFBSSxHQUFHLE9BQU8sQ0FDaEM7QUFHQSxZQUFNO0FBR04sK0JBQXlCO0FBQUE7QUFBQSxJQUczQixlQUFlLENBQUMsUUFBUTtBQUN0QixjQUFRLElBQUksZ0NBQWdDO0FBQzVDLGFBQU8sWUFBWSxLQUFLLFNBQVMsTUFBTTtBQUNyQyxnQkFBUSxJQUFJLDZDQUE2QztBQUN6RCxhQUFLO0FBQUEsT0FDTjtBQUdELCtCQUF5QjtBQUFBO0FBQUEsU0FHckIsV0FBVSxHQUFHO0FBQ2pCLGNBQVEsSUFBSSwyQkFBMkI7QUFFdkMsK0JBQXlCO0FBRXpCLFVBQUksTUFBTSxTQUFTLEdBQUc7QUFDcEIsY0FBTSxZQUFZO0FBQUEsTUFDcEI7QUFBQTtBQUFBLElBR0YsZUFBZSxDQUFDLEtBQUs7QUFDbkIsY0FBUSxJQUFJLHVDQUF1QyxJQUFJLE1BQU07QUFFN0QsK0JBQXlCO0FBR3pCLFVBQUksZ0JBQWdCLEtBQUssQ0FBQyxZQUFZLElBQUksS0FBSyxTQUFTLE9BQU8sQ0FBQyxHQUFHO0FBQ2pFLGdCQUFRLElBQUksdUJBQXVCLElBQUksTUFBTTtBQUM3QztBQUFBLE1BQ0Y7QUFHQSxVQUFJLE9BQU87QUFDVCxnQkFBUSxJQUFJLCtCQUErQjtBQUMzQyxxQkFBYSxLQUFLO0FBQUEsTUFDcEI7QUFDQSxjQUFRLElBQUksd0NBQXdDO0FBQ3BELGNBQVEsV0FBVyxZQUFZO0FBQzdCLGdCQUFRLElBQUksa0NBQWtDO0FBQzlDLGdCQUFRO0FBR1IsaUNBQXlCO0FBQ3pCLGNBQU0sWUFBWTtBQUdsQixpQ0FBeUI7QUFBQSxTQUN4QixHQUFHO0FBR04sWUFBTSxNQUFNO0FBQUE7QUFBQSxJQUdkLFdBQVcsR0FBRztBQUNaLGNBQVEsSUFBSSw0QkFBNEI7QUFFeEMsK0JBQXlCO0FBQUE7QUFBQSxJQUczQixXQUFXLEdBQUc7QUFDWixjQUFRLElBQUksNEJBQTRCO0FBRXhDLCtCQUF5QjtBQUN6QixXQUFLO0FBQUE7QUFBQSxFQUVUO0FBQUE7QUEzT0YsSUFBTSxZQUFZLFVBQVUsSUFBSTtBQVl6QixJQUFNLE9BQU8sQ0FBQyxTQUE2QjtBQU8zQyxJQUFNLFVBQVUsQ0FBQyxXQUFtQixnQkFBa0M7QUFBQSxFQUMzRSxNQUFNO0FBQUEsRUFDTixRQUFRLHNCQUFzQixhQUFhO0FBQzdDO0FBTU8sSUFBTSxRQUFRLENBQUMsWUFBbUM7QUFBQSxFQUN2RCxNQUFNO0FBQUEsRUFDTixRQUFRLE1BQU0sU0FBUyxNQUFNO0FBQy9CO0FBZ05BLElBQWU7IiwKICAiZGVidWdJZCI6ICI0NzMzODJEMzVCMkJCRUZDNjQ3NTZFMjE2NDc1NkUyMSIsCiAgIm5hbWVzIjogW10KfQ==
