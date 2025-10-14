// src/apx/plugins/index.ts
import { existsSync, mkdirSync, writeFileSync, readFileSync } from "fs";
import { join, resolve } from "path";
import { spawn } from "child_process";
import { createHash } from "crypto";
import { generate } from "orval";
function apx(options = {}) {
  const { steps = [], ignore = [] } = options;
  let outDir;
  let timer = null;
  let stopping = false;
  let resolvedIgnores = [];
  let isRunningSteps = false;
  let childProcesses = [];
  function executeShellCommand(command) {
    return new Promise((resolve2, reject) => {
      if (stopping) {
        console.log(`[apx] Skipping command (stopping): ${command}`);
        resolve2();
        return;
      }
      console.log(`[apx] Executing: ${command}`);
      const parts = command.split(/\s+/);
      const cmd = parts[0];
      const args = parts.slice(1);
      const child = spawn(cmd, args, {
        stdio: "inherit",
        shell: true,
        detached: false
      });
      childProcesses.push(child);
      child.on("error", (err) => {
        console.error(`[apx] Process error:`, err);
        reject(err);
      });
      child.on("exit", (code, signal) => {
        childProcesses = childProcesses.filter((p) => p.pid !== child.pid);
        if (signal) {
          console.log(`[apx] Process ${child.pid} exited with signal: ${signal}`);
          resolve2();
        } else if (code !== 0) {
          console.error(`[apx] Process ${child.pid} exited with code: ${code}`);
          reject(new Error(`Command failed with exit code ${code}`));
        } else {
          resolve2();
        }
      });
      if (stopping && child.pid) {
        console.log(`[apx] Killing process ${child.pid} (stopping)`);
        killProcess(child);
      }
    });
  }
  function killProcess(proc) {
    if (!proc.pid)
      return;
    try {
      if (process.platform !== "win32") {
        process.kill(-proc.pid, "SIGTERM");
        console.log(`[apx] Sent SIGTERM to process group -${proc.pid}`);
      } else {
        proc.kill("SIGTERM");
        console.log(`[apx] Sent SIGTERM to process ${proc.pid}`);
      }
    } catch (err) {
      console.error(`[apx] Error killing process ${proc.pid}:`, err);
      try {
        proc.kill("SIGKILL");
      } catch (e) {
      }
    }
  }
  async function executeAction(action) {
    if (stopping) {
      console.log(`[apx] Skipping action (stopping)`);
      return;
    }
    ensureOutDirAndGitignore();
    if (typeof action === "string") {
      await executeShellCommand(action);
    } else {
      if (stopping)
        return;
      await action();
    }
    ensureOutDirAndGitignore();
  }
  async function runAllSteps() {
    if (stopping) {
      console.log(`[apx] Skipping steps (stopping)`);
      return;
    }
    if (isRunningSteps) {
      console.log(`[apx] Steps already running, skipping...`);
      return;
    }
    console.log(`[apx] Running ${steps.length} step(s)...`);
    isRunningSteps = true;
    try {
      for (const step of steps) {
        if (stopping) {
          console.log(`[apx] Stopping during step execution`);
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
      console.log(`[apx] All steps completed`);
    } finally {
      isRunningSteps = false;
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
    if (stopping)
      return;
    console.log(`[apx] Stopping... (${childProcesses.length} child processes)`);
    stopping = true;
    if (timer) {
      clearTimeout(timer);
      timer = null;
    }
    if (childProcesses.length > 0) {
      console.log(`[apx] Killing ${childProcesses.length} child process(es)...`);
      childProcesses.forEach((proc) => {
        if (proc.pid) {
          killProcess(proc);
        }
      });
      childProcesses = [];
    }
    console.log(`[apx] Stopped`);
  }
  function reset() {
    console.log(`[apx] Resetting plugin state`);
    stopping = false;
    timer = null;
    isRunningSteps = false;
    childProcesses = [];
  }
  return {
    name: "apx",
    apply: () => true,
    configResolved(config) {
      outDir = resolve(config.root, config.build.outDir);
      resolvedIgnores = ignore.map((pattern) => resolve(process.cwd(), pattern));
      reset();
      ensureOutDirAndGitignore();
    },
    configureServer(server) {
      server.httpServer?.once("close", () => {
        console.log(`[apx] Server closing, stopping plugin...`);
        stop();
      });
      ensureOutDirAndGitignore();
    },
    async buildStart() {
      ensureOutDirAndGitignore();
      if (steps.length > 0) {
        await runAllSteps();
      }
    },
    handleHotUpdate(ctx) {
      ensureOutDirAndGitignore();
      if (stopping) {
        console.log(`[apx] HMR update ignored (stopping)`);
        return;
      }
      if (resolvedIgnores.some((pattern) => ctx.file.includes(pattern))) {
        return;
      }
      console.log(`[apx] HMR update detected: ${ctx.file}`);
      if (timer) {
        clearTimeout(timer);
      }
      timer = setTimeout(async () => {
        timer = null;
        if (stopping) {
          return;
        }
        ensureOutDirAndGitignore();
        await runAllSteps();
        ensureOutDirAndGitignore();
      }, 100);
      timer.unref();
    },
    writeBundle() {
      ensureOutDirAndGitignore();
    },
    closeBundle() {
      ensureOutDirAndGitignore();
      stop();
    }
  };
}
var specHashCache = new Map;
var Step = (spec) => spec;
var OpenAPI = (appModule, outputPath) => ({
  name: "openapi",
  action: `uv run apx openapi ${appModule} ${outputPath}`
});
var Orval = ({
  input,
  output
}) => ({
  name: "orval",
  action: async () => {
    if (!existsSync(input)) {
      console.warn(`[apx] OpenAPI spec not found at ${input}, skipping Orval generation`);
      return;
    }
    const specContent = readFileSync(input, "utf-8");
    const specHash = createHash("sha256").update(specContent).digest("hex");
    const cachedHash = specHashCache.get(input);
    if (cachedHash === specHash) {
      console.log(`[apx] OpenAPI spec unchanged, skipping Orval generation`);
      return;
    }
    await generate({
      input,
      output
    });
    specHashCache.set(input, specHash);
  }
});
var plugins_default = apx;
export {
  plugins_default as default,
  apx,
  Step,
  Orval,
  OpenAPI
};

//# debugId=1DD2B36E2AECC1B064756E2164756E21
//# sourceMappingURL=data:application/json;base64,ewogICJ2ZXJzaW9uIjogMywKICAic291cmNlcyI6IFsic3JjL2FweC9wbHVnaW5zL2luZGV4LnRzIl0sCiAgInNvdXJjZXNDb250ZW50IjogWwogICAgImltcG9ydCB7IGV4aXN0c1N5bmMsIG1rZGlyU3luYywgd3JpdGVGaWxlU3luYywgcmVhZEZpbGVTeW5jIH0gZnJvbSBcImZzXCI7XG5pbXBvcnQgeyBqb2luLCByZXNvbHZlIH0gZnJvbSBcInBhdGhcIjtcbmltcG9ydCB7IHR5cGUgUGx1Z2luIH0gZnJvbSBcInZpdGVcIjtcbmltcG9ydCB7IHNwYXduLCB0eXBlIENoaWxkUHJvY2VzcyB9IGZyb20gXCJjaGlsZF9wcm9jZXNzXCI7XG5pbXBvcnQgeyBjcmVhdGVIYXNoIH0gZnJvbSBcImNyeXB0b1wiO1xuaW1wb3J0IHsgZ2VuZXJhdGUsIHR5cGUgT3V0cHV0T3B0aW9ucyB9IGZyb20gXCJvcnZhbFwiO1xuXG4vLyBDYWNoZSBmb3IgT3BlbkFQSSBzcGVjIGhhc2hlcyB0byBkZXRlY3QgY2hhbmdlc1xuY29uc3Qgc3BlY0hhc2hDYWNoZSA9IG5ldyBNYXA8c3RyaW5nLCBzdHJpbmc+KCk7XG5cbmV4cG9ydCB0eXBlIFN0ZXBBY3Rpb24gPSBzdHJpbmcgfCAoKCkgPT4gdm9pZCB8IFByb21pc2U8dm9pZD4pO1xuXG5leHBvcnQgdHlwZSBTdGVwU3BlYyA9IHtcbiAgbmFtZTogc3RyaW5nO1xuICBhY3Rpb246IFN0ZXBBY3Rpb247XG59O1xuXG5leHBvcnQgY29uc3QgU3RlcCA9IChzcGVjOiBTdGVwU3BlYyk6IFN0ZXBTcGVjID0+IHNwZWM7XG5cbi8qKlxuICogUHJlZGVmaW5lZCBzdGVwIGZvciBnZW5lcmF0aW5nIE9wZW5BUEkgc2NoZW1hXG4gKiBAcGFyYW0gYXBwTW9kdWxlIC0gVGhlIFB5dGhvbiBtb2R1bGUgcGF0aCAoZS5nLiwgXCJzYW1wbGUuYXBpLmFwcDphcHBcIilcbiAqIEBwYXJhbSBvdXRwdXRQYXRoIC0gV2hlcmUgdG8gd3JpdGUgdGhlIE9wZW5BUEkgSlNPTiBmaWxlXG4gKi9cbmV4cG9ydCBjb25zdCBPcGVuQVBJID0gKGFwcE1vZHVsZTogc3RyaW5nLCBvdXRwdXRQYXRoOiBzdHJpbmcpOiBTdGVwU3BlYyA9PiAoe1xuICBuYW1lOiBcIm9wZW5hcGlcIixcbiAgYWN0aW9uOiBgdXYgcnVuIGFweCBvcGVuYXBpICR7YXBwTW9kdWxlfSAke291dHB1dFBhdGh9YCxcbn0pO1xuXG4vKipcbiAqIFByZWRlZmluZWQgc3RlcCBmb3IgZ2VuZXJhdGluZyBBUEkgY2xpZW50IHdpdGggT3J2YWxcbiAqIFNraXBzIGdlbmVyYXRpb24gaWYgdGhlIE9wZW5BUEkgc3BlYyBoYXNuJ3QgY2hhbmdlZCBzaW5jZSBsYXN0IHJ1blxuICogQHBhcmFtIGlucHV0IC0gUGF0aCB0byB0aGUgT3BlbkFQSSBzcGVjIGZpbGVcbiAqIEBwYXJhbSBvdXRwdXQgLSBPcnZhbCBvdXRwdXQgY29uZmlndXJhdGlvblxuICovXG5leHBvcnQgY29uc3QgT3J2YWwgPSAoe1xuICBpbnB1dCxcbiAgb3V0cHV0LFxufToge1xuICBpbnB1dDogc3RyaW5nO1xuICBvdXRwdXQ6IE91dHB1dE9wdGlvbnM7XG59KTogU3RlcFNwZWMgPT4gKHtcbiAgbmFtZTogXCJvcnZhbFwiLFxuICBhY3Rpb246IGFzeW5jICgpID0+IHtcbiAgICAvLyBDaGVjayBpZiBzcGVjIGZpbGUgZXhpc3RzXG4gICAgaWYgKCFleGlzdHNTeW5jKGlucHV0KSkge1xuICAgICAgY29uc29sZS53YXJuKFxuICAgICAgICBgW2FweF0gT3BlbkFQSSBzcGVjIG5vdCBmb3VuZCBhdCAke2lucHV0fSwgc2tpcHBpbmcgT3J2YWwgZ2VuZXJhdGlvbmAsXG4gICAgICApO1xuICAgICAgcmV0dXJuO1xuICAgIH1cblxuICAgIC8vIFJlYWQgYW5kIGhhc2ggdGhlIHNwZWMgZmlsZVxuICAgIGNvbnN0IHNwZWNDb250ZW50ID0gcmVhZEZpbGVTeW5jKGlucHV0LCBcInV0Zi04XCIpO1xuICAgIGNvbnN0IHNwZWNIYXNoID0gY3JlYXRlSGFzaChcInNoYTI1NlwiKS51cGRhdGUoc3BlY0NvbnRlbnQpLmRpZ2VzdChcImhleFwiKTtcblxuICAgIC8vIENoZWNrIGlmIHNwZWMgaGFzIGNoYW5nZWRcbiAgICBjb25zdCBjYWNoZWRIYXNoID0gc3BlY0hhc2hDYWNoZS5nZXQoaW5wdXQpO1xuICAgIGlmIChjYWNoZWRIYXNoID09PSBzcGVjSGFzaCkge1xuICAgICAgY29uc29sZS5sb2coYFthcHhdIE9wZW5BUEkgc3BlYyB1bmNoYW5nZWQsIHNraXBwaW5nIE9ydmFsIGdlbmVyYXRpb25gKTtcbiAgICAgIHJldHVybjtcbiAgICB9XG5cbiAgICAvLyBHZW5lcmF0ZSBBUEkgY2xpZW50XG4gICAgYXdhaXQgZ2VuZXJhdGUoe1xuICAgICAgaW5wdXQsXG4gICAgICBvdXRwdXQsXG4gICAgfSk7XG5cbiAgICAvLyBVcGRhdGUgY2FjaGVcbiAgICBzcGVjSGFzaENhY2hlLnNldChpbnB1dCwgc3BlY0hhc2gpO1xuICB9LFxufSk7XG5cbmV4cG9ydCBpbnRlcmZhY2UgQXB4UGx1Z2luT3B0aW9ucyB7XG4gIHN0ZXBzPzogU3RlcFNwZWNbXTtcbiAgaWdub3JlPzogc3RyaW5nW107XG59XG5cbmV4cG9ydCBmdW5jdGlvbiBhcHgob3B0aW9uczogQXB4UGx1Z2luT3B0aW9ucyA9IHt9KTogUGx1Z2luIHtcbiAgY29uc3QgeyBzdGVwcyA9IFtdLCBpZ25vcmUgPSBbXSB9ID0gb3B0aW9ucztcblxuICBsZXQgb3V0RGlyOiBzdHJpbmc7XG4gIGxldCB0aW1lcjogTm9kZUpTLlRpbWVvdXQgfCBudWxsID0gbnVsbDtcbiAgbGV0IHN0b3BwaW5nID0gZmFsc2U7XG4gIGxldCByZXNvbHZlZElnbm9yZXM6IHN0cmluZ1tdID0gW107XG4gIGxldCBpc1J1bm5pbmdTdGVwcyA9IGZhbHNlO1xuICBsZXQgY2hpbGRQcm9jZXNzZXM6IENoaWxkUHJvY2Vzc1tdID0gW107XG5cbiAgLyoqXG4gICAqIEV4ZWN1dGVzIGEgc2hlbGwgY29tbWFuZCB1c2luZyBzcGF3biwgd2l0aCBwcm9wZXIgc2lnbmFsIGhhbmRsaW5nXG4gICAqL1xuICBmdW5jdGlvbiBleGVjdXRlU2hlbGxDb21tYW5kKGNvbW1hbmQ6IHN0cmluZyk6IFByb21pc2U8dm9pZD4ge1xuICAgIHJldHVybiBuZXcgUHJvbWlzZSgocmVzb2x2ZSwgcmVqZWN0KSA9PiB7XG4gICAgICBpZiAoc3RvcHBpbmcpIHtcbiAgICAgICAgY29uc29sZS5sb2coYFthcHhdIFNraXBwaW5nIGNvbW1hbmQgKHN0b3BwaW5nKTogJHtjb21tYW5kfWApO1xuICAgICAgICByZXNvbHZlKCk7XG4gICAgICAgIHJldHVybjtcbiAgICAgIH1cblxuICAgICAgY29uc29sZS5sb2coYFthcHhdIEV4ZWN1dGluZzogJHtjb21tYW5kfWApO1xuXG4gICAgICAvLyBQYXJzZSBjb21tYW5kIGludG8gY29tbWFuZCBhbmQgYXJnc1xuICAgICAgY29uc3QgcGFydHMgPSBjb21tYW5kLnNwbGl0KC9cXHMrLyk7XG4gICAgICBjb25zdCBjbWQgPSBwYXJ0c1swXTtcbiAgICAgIGNvbnN0IGFyZ3MgPSBwYXJ0cy5zbGljZSgxKTtcblxuICAgICAgLy8gU3Bhd24gcHJvY2VzcyB3aXRoIHByb3BlciBzaWduYWwgaGFuZGxpbmdcbiAgICAgIGNvbnN0IGNoaWxkID0gc3Bhd24oY21kLCBhcmdzLCB7XG4gICAgICAgIHN0ZGlvOiBcImluaGVyaXRcIiwgLy8gRm9yd2FyZCBzdGRvdXQvc3RkZXJyIHRvIHBhcmVudFxuICAgICAgICBzaGVsbDogdHJ1ZSwgLy8gVXNlIHNoZWxsIGZvciBwcm9wZXIgY29tbWFuZCBwYXJzaW5nXG4gICAgICAgIGRldGFjaGVkOiBmYWxzZSwgLy8gS2VlcCBpbiBzYW1lIHByb2Nlc3MgZ3JvdXAgZm9yIHNpZ25hbCBwcm9wYWdhdGlvblxuICAgICAgfSk7XG5cbiAgICAgIC8vIFRyYWNrIGNoaWxkIHByb2Nlc3MgZm9yIGNsZWFudXBcbiAgICAgIGNoaWxkUHJvY2Vzc2VzLnB1c2goY2hpbGQpO1xuXG4gICAgICBjaGlsZC5vbihcImVycm9yXCIsIChlcnIpID0+IHtcbiAgICAgICAgY29uc29sZS5lcnJvcihgW2FweF0gUHJvY2VzcyBlcnJvcjpgLCBlcnIpO1xuICAgICAgICByZWplY3QoZXJyKTtcbiAgICAgIH0pO1xuXG4gICAgICBjaGlsZC5vbihcImV4aXRcIiwgKGNvZGUsIHNpZ25hbCkgPT4ge1xuICAgICAgICAvLyBSZW1vdmUgZnJvbSB0cmFja2luZ1xuICAgICAgICBjaGlsZFByb2Nlc3NlcyA9IGNoaWxkUHJvY2Vzc2VzLmZpbHRlcigocCkgPT4gcC5waWQgIT09IGNoaWxkLnBpZCk7XG5cbiAgICAgICAgaWYgKHNpZ25hbCkge1xuICAgICAgICAgIGNvbnNvbGUubG9nKFxuICAgICAgICAgICAgYFthcHhdIFByb2Nlc3MgJHtjaGlsZC5waWR9IGV4aXRlZCB3aXRoIHNpZ25hbDogJHtzaWduYWx9YCxcbiAgICAgICAgICApO1xuICAgICAgICAgIHJlc29sdmUoKTsgLy8gVHJlYXQgc2lnbmFsIHRlcm1pbmF0aW9uIGFzIHN1Y2Nlc3MgZm9yIGNsZWFudXAgc2NlbmFyaW9zXG4gICAgICAgIH0gZWxzZSBpZiAoY29kZSAhPT0gMCkge1xuICAgICAgICAgIGNvbnNvbGUuZXJyb3IoYFthcHhdIFByb2Nlc3MgJHtjaGlsZC5waWR9IGV4aXRlZCB3aXRoIGNvZGU6ICR7Y29kZX1gKTtcbiAgICAgICAgICByZWplY3QobmV3IEVycm9yKGBDb21tYW5kIGZhaWxlZCB3aXRoIGV4aXQgY29kZSAke2NvZGV9YCkpO1xuICAgICAgICB9IGVsc2Uge1xuICAgICAgICAgIHJlc29sdmUoKTtcbiAgICAgICAgfVxuICAgICAgfSk7XG5cbiAgICAgIC8vIElmIHdlJ3JlIHN0b3BwaW5nLCBraWxsIHRoZSBwcm9jZXNzIGltbWVkaWF0ZWx5XG4gICAgICBpZiAoc3RvcHBpbmcgJiYgY2hpbGQucGlkKSB7XG4gICAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSBLaWxsaW5nIHByb2Nlc3MgJHtjaGlsZC5waWR9IChzdG9wcGluZylgKTtcbiAgICAgICAga2lsbFByb2Nlc3MoY2hpbGQpO1xuICAgICAgfVxuICAgIH0pO1xuICB9XG5cbiAgLyoqXG4gICAqIEtpbGxzIGEgcHJvY2VzcyBhbmQgYWxsIGl0cyBjaGlsZHJlblxuICAgKi9cbiAgZnVuY3Rpb24ga2lsbFByb2Nlc3MocHJvYzogQ2hpbGRQcm9jZXNzKTogdm9pZCB7XG4gICAgaWYgKCFwcm9jLnBpZCkgcmV0dXJuO1xuXG4gICAgdHJ5IHtcbiAgICAgIC8vIE9uIFVuaXgtbGlrZSBzeXN0ZW1zLCBraWxsIHRoZSBwcm9jZXNzIGdyb3VwXG4gICAgICAvLyBOZWdhdGl2ZSBQSUQga2lsbHMgdGhlIGVudGlyZSBwcm9jZXNzIGdyb3VwXG4gICAgICBpZiAocHJvY2Vzcy5wbGF0Zm9ybSAhPT0gXCJ3aW4zMlwiKSB7XG4gICAgICAgIHByb2Nlc3Mua2lsbCgtcHJvYy5waWQsIFwiU0lHVEVSTVwiKTtcbiAgICAgICAgY29uc29sZS5sb2coYFthcHhdIFNlbnQgU0lHVEVSTSB0byBwcm9jZXNzIGdyb3VwIC0ke3Byb2MucGlkfWApO1xuICAgICAgfSBlbHNlIHtcbiAgICAgICAgLy8gT24gV2luZG93cywganVzdCBraWxsIHRoZSBwcm9jZXNzXG4gICAgICAgIHByb2Mua2lsbChcIlNJR1RFUk1cIik7XG4gICAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSBTZW50IFNJR1RFUk0gdG8gcHJvY2VzcyAke3Byb2MucGlkfWApO1xuICAgICAgfVxuICAgIH0gY2F0Y2ggKGVycikge1xuICAgICAgY29uc29sZS5lcnJvcihgW2FweF0gRXJyb3Iga2lsbGluZyBwcm9jZXNzICR7cHJvYy5waWR9OmAsIGVycik7XG4gICAgICAvLyBUcnkgZm9yY2VmdWwga2lsbCBhcyBmYWxsYmFja1xuICAgICAgdHJ5IHtcbiAgICAgICAgcHJvYy5raWxsKFwiU0lHS0lMTFwiKTtcbiAgICAgIH0gY2F0Y2ggKGUpIHtcbiAgICAgICAgLy8gSWdub3JlIGVycm9ycyBvbiBmb3JjZWZ1bCBraWxsXG4gICAgICB9XG4gICAgfVxuICB9XG5cbiAgYXN5bmMgZnVuY3Rpb24gZXhlY3V0ZUFjdGlvbihhY3Rpb246IFN0ZXBBY3Rpb24pOiBQcm9taXNlPHZvaWQ+IHtcbiAgICBpZiAoc3RvcHBpbmcpIHtcbiAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSBTa2lwcGluZyBhY3Rpb24gKHN0b3BwaW5nKWApO1xuICAgICAgcmV0dXJuO1xuICAgIH1cblxuICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgIGlmICh0eXBlb2YgYWN0aW9uID09PSBcInN0cmluZ1wiKSB7XG4gICAgICAvLyBFeGVjdXRlIGFzIHNoZWxsIGNvbW1hbmRcbiAgICAgIGF3YWl0IGV4ZWN1dGVTaGVsbENvbW1hbmQoYWN0aW9uKTtcbiAgICB9IGVsc2Uge1xuICAgICAgLy8gRXhlY3V0ZSBhcyBmdW5jdGlvblxuICAgICAgaWYgKHN0b3BwaW5nKSByZXR1cm47XG4gICAgICBhd2FpdCBhY3Rpb24oKTtcbiAgICB9XG4gICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gIH1cblxuICBhc3luYyBmdW5jdGlvbiBydW5BbGxTdGVwcygpOiBQcm9taXNlPHZvaWQ+IHtcbiAgICBpZiAoc3RvcHBpbmcpIHtcbiAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSBTa2lwcGluZyBzdGVwcyAoc3RvcHBpbmcpYCk7XG4gICAgICByZXR1cm47XG4gICAgfVxuXG4gICAgaWYgKGlzUnVubmluZ1N0ZXBzKSB7XG4gICAgICBjb25zb2xlLmxvZyhgW2FweF0gU3RlcHMgYWxyZWFkeSBydW5uaW5nLCBza2lwcGluZy4uLmApO1xuICAgICAgcmV0dXJuO1xuICAgIH1cblxuICAgIGNvbnNvbGUubG9nKGBbYXB4XSBSdW5uaW5nICR7c3RlcHMubGVuZ3RofSBzdGVwKHMpLi4uYCk7XG4gICAgaXNSdW5uaW5nU3RlcHMgPSB0cnVlO1xuXG4gICAgdHJ5IHtcbiAgICAgIGZvciAoY29uc3Qgc3RlcCBvZiBzdGVwcykge1xuICAgICAgICBpZiAoc3RvcHBpbmcpIHtcbiAgICAgICAgICBjb25zb2xlLmxvZyhgW2FweF0gU3RvcHBpbmcgZHVyaW5nIHN0ZXAgZXhlY3V0aW9uYCk7XG4gICAgICAgICAgYnJlYWs7XG4gICAgICAgIH1cbiAgICAgICAgY29uc3Qgc3RhcnQgPSBEYXRlLm5vdygpO1xuICAgICAgICB0cnkge1xuICAgICAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSAke3N0ZXAubmFtZX0g4o+zYCk7XG4gICAgICAgICAgYXdhaXQgZXhlY3V0ZUFjdGlvbihzdGVwLmFjdGlvbik7XG4gICAgICAgICAgY29uc29sZS5sb2coYFthcHhdICR7c3RlcC5uYW1lfSDinJMgKCR7RGF0ZS5ub3coKSAtIHN0YXJ0fSBtcylgKTtcbiAgICAgICAgfSBjYXRjaCAoZXJyKSB7XG4gICAgICAgICAgY29uc29sZS5lcnJvcihgW2FweF0gJHtzdGVwLm5hbWV9IOKcl2AsIGVycik7XG4gICAgICAgICAgdGhyb3cgZXJyO1xuICAgICAgICB9XG4gICAgICB9XG4gICAgICBjb25zb2xlLmxvZyhgW2FweF0gQWxsIHN0ZXBzIGNvbXBsZXRlZGApO1xuICAgIH0gZmluYWxseSB7XG4gICAgICBpc1J1bm5pbmdTdGVwcyA9IGZhbHNlO1xuICAgIH1cbiAgfVxuXG4gIC8qKlxuICAgKiBFbnN1cmVzIHRoZSBvdXRwdXQgZGlyZWN0b3J5IGV4aXN0cyBhbmQgY29udGFpbnMgYSAuZ2l0aWdub3JlIGZpbGUuXG4gICAqIFRoaXMgaXMgY2FsbGVkIGF0IG11bHRpcGxlIHBvaW50cyB0byBndWFyYW50ZWUgdGhlIGRpcmVjdG9yeSBpcyBhbHdheXMgcHJlc2VudC5cbiAgICovXG4gIGZ1bmN0aW9uIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpOiB2b2lkIHtcbiAgICBpZiAoIW91dERpcikge1xuICAgICAgY29uc29sZS5lcnJvcihgW2FweF0gb3V0RGlyIGlzIG5vdCBzZXRgKTtcbiAgICAgIHJldHVybjtcbiAgICB9XG5cbiAgICB0cnkge1xuICAgICAgLy8gQWx3YXlzIGVuc3VyZSB0aGUgb3V0cHV0IGRpcmVjdG9yeSBleGlzdHNcbiAgICAgIGlmICghZXhpc3RzU3luYyhvdXREaXIpKSB7XG4gICAgICAgIG1rZGlyU3luYyhvdXREaXIsIHsgcmVjdXJzaXZlOiB0cnVlIH0pO1xuICAgICAgfVxuXG4gICAgICAvLyBBbHdheXMgZW5zdXJlIC5naXRpZ25vcmUgZXhpc3RzIGluIG91dHB1dCBkaXJlY3RvcnlcbiAgICAgIGNvbnN0IGdpdGlnbm9yZVBhdGggPSBqb2luKG91dERpciwgXCIuZ2l0aWdub3JlXCIpO1xuICAgICAgaWYgKCFleGlzdHNTeW5jKGdpdGlnbm9yZVBhdGgpKSB7XG4gICAgICAgIHdyaXRlRmlsZVN5bmMoZ2l0aWdub3JlUGF0aCwgXCIqXFxuXCIpO1xuICAgICAgfVxuICAgIH0gY2F0Y2ggKGVycikge1xuICAgICAgY29uc29sZS5lcnJvcihgW2FweF0gZmFpbGVkIHRvIGVuc3VyZSBvdXRwdXQgZGlyZWN0b3J5OmAsIGVycik7XG4gICAgfVxuICB9XG5cbiAgZnVuY3Rpb24gc3RvcCgpOiB2b2lkIHtcbiAgICBpZiAoc3RvcHBpbmcpIHJldHVybjtcbiAgICBjb25zb2xlLmxvZyhgW2FweF0gU3RvcHBpbmcuLi4gKCR7Y2hpbGRQcm9jZXNzZXMubGVuZ3RofSBjaGlsZCBwcm9jZXNzZXMpYCk7XG4gICAgc3RvcHBpbmcgPSB0cnVlO1xuXG4gICAgLy8gQ2xlYXIgYW55IHBlbmRpbmcgdGltZXJzXG4gICAgaWYgKHRpbWVyKSB7XG4gICAgICBjbGVhclRpbWVvdXQodGltZXIpO1xuICAgICAgdGltZXIgPSBudWxsO1xuICAgIH1cblxuICAgIC8vIEtpbGwgYWxsIHRyYWNrZWQgY2hpbGQgcHJvY2Vzc2VzXG4gICAgaWYgKGNoaWxkUHJvY2Vzc2VzLmxlbmd0aCA+IDApIHtcbiAgICAgIGNvbnNvbGUubG9nKFxuICAgICAgICBgW2FweF0gS2lsbGluZyAke2NoaWxkUHJvY2Vzc2VzLmxlbmd0aH0gY2hpbGQgcHJvY2VzcyhlcykuLi5gLFxuICAgICAgKTtcbiAgICAgIGNoaWxkUHJvY2Vzc2VzLmZvckVhY2goKHByb2MpID0+IHtcbiAgICAgICAgaWYgKHByb2MucGlkKSB7XG4gICAgICAgICAga2lsbFByb2Nlc3MocHJvYyk7XG4gICAgICAgIH1cbiAgICAgIH0pO1xuICAgICAgY2hpbGRQcm9jZXNzZXMgPSBbXTtcbiAgICB9XG5cbiAgICBjb25zb2xlLmxvZyhgW2FweF0gU3RvcHBlZGApO1xuICB9XG5cbiAgZnVuY3Rpb24gcmVzZXQoKTogdm9pZCB7XG4gICAgY29uc29sZS5sb2coYFthcHhdIFJlc2V0dGluZyBwbHVnaW4gc3RhdGVgKTtcbiAgICBzdG9wcGluZyA9IGZhbHNlO1xuICAgIHRpbWVyID0gbnVsbDtcbiAgICBpc1J1bm5pbmdTdGVwcyA9IGZhbHNlO1xuICAgIGNoaWxkUHJvY2Vzc2VzID0gW107XG4gIH1cblxuICByZXR1cm4ge1xuICAgIG5hbWU6IFwiYXB4XCIsXG4gICAgYXBwbHk6ICgpID0+IHRydWUsXG5cbiAgICBjb25maWdSZXNvbHZlZChjb25maWcpIHtcbiAgICAgIG91dERpciA9IHJlc29sdmUoY29uZmlnLnJvb3QsIGNvbmZpZy5idWlsZC5vdXREaXIpO1xuICAgICAgcmVzb2x2ZWRJZ25vcmVzID0gaWdub3JlLm1hcCgocGF0dGVybikgPT5cbiAgICAgICAgcmVzb2x2ZShwcm9jZXNzLmN3ZCgpLCBwYXR0ZXJuKSxcbiAgICAgICk7XG5cbiAgICAgIC8vIFJlc2V0IHN0YXRlIGZvciBuZXcgYnVpbGRcbiAgICAgIHJlc2V0KCk7XG5cbiAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIGFzIHNvb24gYXMgd2Uga25vdyB0aGUgb3V0RGlyXG4gICAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcbiAgICB9LFxuXG4gICAgY29uZmlndXJlU2VydmVyKHNlcnZlcikge1xuICAgICAgLy8gTGV0IFZpdGUgaGFuZGxlIFNJR0lOVC9TSUdURVJNIC0gd2UnbGwgY2xlYW4gdXAgdmlhIHNlcnZlci5jbG9zZSBhbmQgY2xvc2VCdW5kbGVcbiAgICAgIC8vIERPTidUIGFkZCBzaWduYWwgaGFuZGxlcnMgaGVyZSBhcyB0aGV5IGludGVyZmVyZSB3aXRoIFZpdGUncyBzaWduYWwgaGFuZGxpbmdcbiAgICAgIC8vIFNlZTogaHR0cHM6Ly9naXRodWIuY29tL3ZpdGVqcy92aXRlL2lzc3Vlcy8xMTQzNFxuICAgICAgc2VydmVyLmh0dHBTZXJ2ZXI/Lm9uY2UoXCJjbG9zZVwiLCAoKSA9PiB7XG4gICAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSBTZXJ2ZXIgY2xvc2luZywgc3RvcHBpbmcgcGx1Z2luLi4uYCk7XG4gICAgICAgIHN0b3AoKTtcbiAgICAgIH0pO1xuXG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyB3aGVuIHNlcnZlciBzdGFydHNcbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgIH0sXG5cbiAgICBhc3luYyBidWlsZFN0YXJ0KCkge1xuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgYmVmb3JlIGJ1aWxkIHN0YXJ0c1xuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG5cbiAgICAgIGlmIChzdGVwcy5sZW5ndGggPiAwKSB7XG4gICAgICAgIGF3YWl0IHJ1bkFsbFN0ZXBzKCk7XG4gICAgICB9XG4gICAgfSxcblxuICAgIGhhbmRsZUhvdFVwZGF0ZShjdHgpIHtcbiAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIG9uIGV2ZXJ5IEhNUiB1cGRhdGVcbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuXG4gICAgICAvLyBEb24ndCB0cmlnZ2VyIHVwZGF0ZXMgaWYgc3RvcHBpbmdcbiAgICAgIGlmIChzdG9wcGluZykge1xuICAgICAgICBjb25zb2xlLmxvZyhgW2FweF0gSE1SIHVwZGF0ZSBpZ25vcmVkIChzdG9wcGluZylgKTtcbiAgICAgICAgcmV0dXJuO1xuICAgICAgfVxuXG4gICAgICAvLyBDaGVjayBpZiBmaWxlIHNob3VsZCBiZSBpZ25vcmVkXG4gICAgICBpZiAocmVzb2x2ZWRJZ25vcmVzLnNvbWUoKHBhdHRlcm4pID0+IGN0eC5maWxlLmluY2x1ZGVzKHBhdHRlcm4pKSkge1xuICAgICAgICByZXR1cm47XG4gICAgICB9XG5cbiAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSBITVIgdXBkYXRlIGRldGVjdGVkOiAke2N0eC5maWxlfWApO1xuXG4gICAgICAvLyBEZWJvdW5jZSBzdGVwIGV4ZWN1dGlvbiBvbiBITVIgdXBkYXRlc1xuICAgICAgaWYgKHRpbWVyKSB7XG4gICAgICAgIGNsZWFyVGltZW91dCh0aW1lcik7XG4gICAgICB9XG5cbiAgICAgIHRpbWVyID0gc2V0VGltZW91dChhc3luYyAoKSA9PiB7XG4gICAgICAgIHRpbWVyID0gbnVsbDtcblxuICAgICAgICAvLyBEb3VibGUtY2hlY2sgd2UncmUgbm90IHN0b3BwaW5nIGJlZm9yZSBydW5uaW5nXG4gICAgICAgIGlmIChzdG9wcGluZykge1xuICAgICAgICAgIHJldHVybjtcbiAgICAgICAgfVxuICAgICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBiZWZvcmUgcnVubmluZyBzdGVwc1xuICAgICAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcbiAgICAgICAgYXdhaXQgcnVuQWxsU3RlcHMoKTtcblxuICAgICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBhZnRlciBydW5uaW5nIHN0ZXBzXG4gICAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgICAgfSwgMTAwKTtcblxuICAgICAgLy8gQWxsb3cgdGhlIHByb2Nlc3MgdG8gZXhpdCBldmVuIGlmIHRoaXMgdGltZXIgaXMgcGVuZGluZ1xuICAgICAgdGltZXIudW5yZWYoKTtcbiAgICB9LFxuXG4gICAgd3JpdGVCdW5kbGUoKSB7XG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBhZnRlciBmaWxlcyBhcmUgd3JpdHRlblxuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgfSxcblxuICAgIGNsb3NlQnVuZGxlKCkge1xuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgb25lIGZpbmFsIHRpbWVcbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgICAgc3RvcCgpO1xuICAgIH0sXG4gIH07XG59XG5cbi8vIERlZmF1bHQgZXhwb3J0IGZvciBjb252ZW5pZW5jZTogaW1wb3J0IGFweCBmcm9tIFwiYXB4XCJcbmV4cG9ydCBkZWZhdWx0IGFweDtcbiIKICBdLAogICJtYXBwaW5ncyI6ICI7QUFBQTtBQUNBO0FBRUE7QUFDQTtBQUNBO0FBMEVPLFNBQVMsR0FBRyxDQUFDLFVBQTRCLENBQUMsR0FBVztBQUMxRCxVQUFRLFFBQVEsQ0FBQyxHQUFHLFNBQVMsQ0FBQyxNQUFNO0FBRXBDLE1BQUk7QUFDSixNQUFJLFFBQStCO0FBQ25DLE1BQUksV0FBVztBQUNmLE1BQUksa0JBQTRCLENBQUM7QUFDakMsTUFBSSxpQkFBaUI7QUFDckIsTUFBSSxpQkFBaUMsQ0FBQztBQUt0QyxXQUFTLG1CQUFtQixDQUFDLFNBQWdDO0FBQzNELFdBQU8sSUFBSSxRQUFRLENBQUMsVUFBUyxXQUFXO0FBQ3RDLFVBQUksVUFBVTtBQUNaLGdCQUFRLElBQUksc0NBQXNDLFNBQVM7QUFDM0QsaUJBQVE7QUFDUjtBQUFBLE1BQ0Y7QUFFQSxjQUFRLElBQUksb0JBQW9CLFNBQVM7QUFHekMsWUFBTSxRQUFRLFFBQVEsTUFBTSxLQUFLO0FBQ2pDLFlBQU0sTUFBTSxNQUFNO0FBQ2xCLFlBQU0sT0FBTyxNQUFNLE1BQU0sQ0FBQztBQUcxQixZQUFNLFFBQVEsTUFBTSxLQUFLLE1BQU07QUFBQSxRQUM3QixPQUFPO0FBQUEsUUFDUCxPQUFPO0FBQUEsUUFDUCxVQUFVO0FBQUEsTUFDWixDQUFDO0FBR0QscUJBQWUsS0FBSyxLQUFLO0FBRXpCLFlBQU0sR0FBRyxTQUFTLENBQUMsUUFBUTtBQUN6QixnQkFBUSxNQUFNLHdCQUF3QixHQUFHO0FBQ3pDLGVBQU8sR0FBRztBQUFBLE9BQ1g7QUFFRCxZQUFNLEdBQUcsUUFBUSxDQUFDLE1BQU0sV0FBVztBQUVqQyx5QkFBaUIsZUFBZSxPQUFPLENBQUMsTUFBTSxFQUFFLFFBQVEsTUFBTSxHQUFHO0FBRWpFLFlBQUksUUFBUTtBQUNWLGtCQUFRLElBQ04saUJBQWlCLE1BQU0sMkJBQTJCLFFBQ3BEO0FBQ0EsbUJBQVE7QUFBQSxRQUNWLFdBQVcsU0FBUyxHQUFHO0FBQ3JCLGtCQUFRLE1BQU0saUJBQWlCLE1BQU0seUJBQXlCLE1BQU07QUFDcEUsaUJBQU8sSUFBSSxNQUFNLGlDQUFpQyxNQUFNLENBQUM7QUFBQSxRQUMzRCxPQUFPO0FBQ0wsbUJBQVE7QUFBQTtBQUFBLE9BRVg7QUFHRCxVQUFJLFlBQVksTUFBTSxLQUFLO0FBQ3pCLGdCQUFRLElBQUkseUJBQXlCLE1BQU0sZ0JBQWdCO0FBQzNELG9CQUFZLEtBQUs7QUFBQSxNQUNuQjtBQUFBLEtBQ0Q7QUFBQTtBQU1ILFdBQVMsV0FBVyxDQUFDLE1BQTBCO0FBQzdDLFNBQUssS0FBSztBQUFLO0FBRWYsUUFBSTtBQUdGLFVBQUksUUFBUSxhQUFhLFNBQVM7QUFDaEMsZ0JBQVEsTUFBTSxLQUFLLEtBQUssU0FBUztBQUNqQyxnQkFBUSxJQUFJLHdDQUF3QyxLQUFLLEtBQUs7QUFBQSxNQUNoRSxPQUFPO0FBRUwsYUFBSyxLQUFLLFNBQVM7QUFDbkIsZ0JBQVEsSUFBSSxpQ0FBaUMsS0FBSyxLQUFLO0FBQUE7QUFBQSxhQUVsRCxLQUFQO0FBQ0EsY0FBUSxNQUFNLCtCQUErQixLQUFLLFFBQVEsR0FBRztBQUU3RCxVQUFJO0FBQ0YsYUFBSyxLQUFLLFNBQVM7QUFBQSxlQUNaLEdBQVA7QUFBQTtBQUFBO0FBQUE7QUFNTixpQkFBZSxhQUFhLENBQUMsUUFBbUM7QUFDOUQsUUFBSSxVQUFVO0FBQ1osY0FBUSxJQUFJLGtDQUFrQztBQUM5QztBQUFBLElBQ0Y7QUFFQSw2QkFBeUI7QUFDekIsZUFBVyxXQUFXLFVBQVU7QUFFOUIsWUFBTSxvQkFBb0IsTUFBTTtBQUFBLElBQ2xDLE9BQU87QUFFTCxVQUFJO0FBQVU7QUFDZCxZQUFNLE9BQU87QUFBQTtBQUVmLDZCQUF5QjtBQUFBO0FBRzNCLGlCQUFlLFdBQVcsR0FBa0I7QUFDMUMsUUFBSSxVQUFVO0FBQ1osY0FBUSxJQUFJLGlDQUFpQztBQUM3QztBQUFBLElBQ0Y7QUFFQSxRQUFJLGdCQUFnQjtBQUNsQixjQUFRLElBQUksMENBQTBDO0FBQ3REO0FBQUEsSUFDRjtBQUVBLFlBQVEsSUFBSSxpQkFBaUIsTUFBTSxtQkFBbUI7QUFDdEQscUJBQWlCO0FBRWpCLFFBQUk7QUFDRixpQkFBVyxRQUFRLE9BQU87QUFDeEIsWUFBSSxVQUFVO0FBQ1osa0JBQVEsSUFBSSxzQ0FBc0M7QUFDbEQ7QUFBQSxRQUNGO0FBQ0EsY0FBTSxRQUFRLEtBQUssSUFBSTtBQUN2QixZQUFJO0FBQ0Ysa0JBQVEsSUFBSSxTQUFTLEtBQUssYUFBTztBQUNqQyxnQkFBTSxjQUFjLEtBQUssTUFBTTtBQUMvQixrQkFBUSxJQUFJLFNBQVMsS0FBSyxnQkFBVSxLQUFLLElBQUksSUFBSSxXQUFXO0FBQUEsaUJBQ3JELEtBQVA7QUFDQSxrQkFBUSxNQUFNLFNBQVMsS0FBSyxlQUFTLEdBQUc7QUFDeEMsZ0JBQU07QUFBQTtBQUFBLE1BRVY7QUFDQSxjQUFRLElBQUksMkJBQTJCO0FBQUEsY0FDdkM7QUFDQSx1QkFBaUI7QUFBQTtBQUFBO0FBUXJCLFdBQVMsd0JBQXdCLEdBQVM7QUFDeEMsU0FBSyxRQUFRO0FBQ1gsY0FBUSxNQUFNLHlCQUF5QjtBQUN2QztBQUFBLElBQ0Y7QUFFQSxRQUFJO0FBRUYsV0FBSyxXQUFXLE1BQU0sR0FBRztBQUN2QixrQkFBVSxRQUFRLEVBQUUsV0FBVyxLQUFLLENBQUM7QUFBQSxNQUN2QztBQUdBLFlBQU0sZ0JBQWdCLEtBQUssUUFBUSxZQUFZO0FBQy9DLFdBQUssV0FBVyxhQUFhLEdBQUc7QUFDOUIsc0JBQWMsZUFBZSxLQUFLO0FBQUEsTUFDcEM7QUFBQSxhQUNPLEtBQVA7QUFDQSxjQUFRLE1BQU0sNENBQTRDLEdBQUc7QUFBQTtBQUFBO0FBSWpFLFdBQVMsSUFBSSxHQUFTO0FBQ3BCLFFBQUk7QUFBVTtBQUNkLFlBQVEsSUFBSSxzQkFBc0IsZUFBZSx5QkFBeUI7QUFDMUUsZUFBVztBQUdYLFFBQUksT0FBTztBQUNULG1CQUFhLEtBQUs7QUFDbEIsY0FBUTtBQUFBLElBQ1Y7QUFHQSxRQUFJLGVBQWUsU0FBUyxHQUFHO0FBQzdCLGNBQVEsSUFDTixpQkFBaUIsZUFBZSw2QkFDbEM7QUFDQSxxQkFBZSxRQUFRLENBQUMsU0FBUztBQUMvQixZQUFJLEtBQUssS0FBSztBQUNaLHNCQUFZLElBQUk7QUFBQSxRQUNsQjtBQUFBLE9BQ0Q7QUFDRCx1QkFBaUIsQ0FBQztBQUFBLElBQ3BCO0FBRUEsWUFBUSxJQUFJLGVBQWU7QUFBQTtBQUc3QixXQUFTLEtBQUssR0FBUztBQUNyQixZQUFRLElBQUksOEJBQThCO0FBQzFDLGVBQVc7QUFDWCxZQUFRO0FBQ1IscUJBQWlCO0FBQ2pCLHFCQUFpQixDQUFDO0FBQUE7QUFHcEIsU0FBTztBQUFBLElBQ0wsTUFBTTtBQUFBLElBQ04sT0FBTyxNQUFNO0FBQUEsSUFFYixjQUFjLENBQUMsUUFBUTtBQUNyQixlQUFTLFFBQVEsT0FBTyxNQUFNLE9BQU8sTUFBTSxNQUFNO0FBQ2pELHdCQUFrQixPQUFPLElBQUksQ0FBQyxZQUM1QixRQUFRLFFBQVEsSUFBSSxHQUFHLE9BQU8sQ0FDaEM7QUFHQSxZQUFNO0FBR04sK0JBQXlCO0FBQUE7QUFBQSxJQUczQixlQUFlLENBQUMsUUFBUTtBQUl0QixhQUFPLFlBQVksS0FBSyxTQUFTLE1BQU07QUFDckMsZ0JBQVEsSUFBSSwwQ0FBMEM7QUFDdEQsYUFBSztBQUFBLE9BQ047QUFHRCwrQkFBeUI7QUFBQTtBQUFBLFNBR3JCLFdBQVUsR0FBRztBQUVqQiwrQkFBeUI7QUFFekIsVUFBSSxNQUFNLFNBQVMsR0FBRztBQUNwQixjQUFNLFlBQVk7QUFBQSxNQUNwQjtBQUFBO0FBQUEsSUFHRixlQUFlLENBQUMsS0FBSztBQUVuQiwrQkFBeUI7QUFHekIsVUFBSSxVQUFVO0FBQ1osZ0JBQVEsSUFBSSxxQ0FBcUM7QUFDakQ7QUFBQSxNQUNGO0FBR0EsVUFBSSxnQkFBZ0IsS0FBSyxDQUFDLFlBQVksSUFBSSxLQUFLLFNBQVMsT0FBTyxDQUFDLEdBQUc7QUFDakU7QUFBQSxNQUNGO0FBRUEsY0FBUSxJQUFJLDhCQUE4QixJQUFJLE1BQU07QUFHcEQsVUFBSSxPQUFPO0FBQ1QscUJBQWEsS0FBSztBQUFBLE1BQ3BCO0FBRUEsY0FBUSxXQUFXLFlBQVk7QUFDN0IsZ0JBQVE7QUFHUixZQUFJLFVBQVU7QUFDWjtBQUFBLFFBQ0Y7QUFFQSxpQ0FBeUI7QUFDekIsY0FBTSxZQUFZO0FBR2xCLGlDQUF5QjtBQUFBLFNBQ3hCLEdBQUc7QUFHTixZQUFNLE1BQU07QUFBQTtBQUFBLElBR2QsV0FBVyxHQUFHO0FBRVosK0JBQXlCO0FBQUE7QUFBQSxJQUczQixXQUFXLEdBQUc7QUFFWiwrQkFBeUI7QUFDekIsV0FBSztBQUFBO0FBQUEsRUFFVDtBQUFBO0FBcFhGLElBQU0sZ0JBQWdCLElBQUk7QUFTbkIsSUFBTSxPQUFPLENBQUMsU0FBNkI7QUFPM0MsSUFBTSxVQUFVLENBQUMsV0FBbUIsZ0JBQWtDO0FBQUEsRUFDM0UsTUFBTTtBQUFBLEVBQ04sUUFBUSxzQkFBc0IsYUFBYTtBQUM3QztBQVFPLElBQU0sUUFBUTtBQUFBLEVBQ25CO0FBQUEsRUFDQTtBQUFBLE9BSWU7QUFBQSxFQUNmLE1BQU07QUFBQSxFQUNOLFFBQVEsWUFBWTtBQUVsQixTQUFLLFdBQVcsS0FBSyxHQUFHO0FBQ3RCLGNBQVEsS0FDTixtQ0FBbUMsa0NBQ3JDO0FBQ0E7QUFBQSxJQUNGO0FBR0EsVUFBTSxjQUFjLGFBQWEsT0FBTyxPQUFPO0FBQy9DLFVBQU0sV0FBVyxXQUFXLFFBQVEsRUFBRSxPQUFPLFdBQVcsRUFBRSxPQUFPLEtBQUs7QUFHdEUsVUFBTSxhQUFhLGNBQWMsSUFBSSxLQUFLO0FBQzFDLFFBQUksZUFBZSxVQUFVO0FBQzNCLGNBQVEsSUFBSSx5REFBeUQ7QUFDckU7QUFBQSxJQUNGO0FBR0EsVUFBTSxTQUFTO0FBQUEsTUFDYjtBQUFBLE1BQ0E7QUFBQSxJQUNGLENBQUM7QUFHRCxrQkFBYyxJQUFJLE9BQU8sUUFBUTtBQUFBO0FBRXJDO0FBd1RBLElBQWU7IiwKICAiZGVidWdJZCI6ICIxREQyQjM2RTJBRUNDMUIwNjQ3NTZFMjE2NDc1NkUyMSIsCiAgIm5hbWVzIjogW10KfQ==
