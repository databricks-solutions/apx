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
      console.log(`[apx] Started process PID: ${child.pid}`);
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
          console.log(`[apx] Process ${child.pid} completed successfully`);
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
        console.log(`[apx] HMR update ignored (matches ignore pattern): ${ctx.file}`);
        return;
      }
      console.log(`[apx] HMR update detected: ${ctx.file}`);
      if (timer) {
        clearTimeout(timer);
        console.log(`[apx] HMR debounced (resetting timer)`);
      }
      timer = setTimeout(async () => {
        timer = null;
        if (stopping) {
          console.log(`[apx] HMR callback cancelled (stopping)`);
          return;
        }
        console.log(`[apx] HMR triggered step execution`);
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

//# debugId=80C1DE8FBFE0E96664756E2164756E21
//# sourceMappingURL=data:application/json;base64,ewogICJ2ZXJzaW9uIjogMywKICAic291cmNlcyI6IFsic3JjL2FweC9wbHVnaW5zL2luZGV4LnRzIl0sCiAgInNvdXJjZXNDb250ZW50IjogWwogICAgImltcG9ydCB7IGV4aXN0c1N5bmMsIG1rZGlyU3luYywgd3JpdGVGaWxlU3luYywgcmVhZEZpbGVTeW5jIH0gZnJvbSBcImZzXCI7XG5pbXBvcnQgeyBqb2luLCByZXNvbHZlIH0gZnJvbSBcInBhdGhcIjtcbmltcG9ydCB7IHR5cGUgUGx1Z2luIH0gZnJvbSBcInZpdGVcIjtcbmltcG9ydCB7IHNwYXduLCB0eXBlIENoaWxkUHJvY2VzcyB9IGZyb20gXCJjaGlsZF9wcm9jZXNzXCI7XG5pbXBvcnQgeyBjcmVhdGVIYXNoIH0gZnJvbSBcImNyeXB0b1wiO1xuaW1wb3J0IHsgZ2VuZXJhdGUsIHR5cGUgT3V0cHV0T3B0aW9ucyB9IGZyb20gXCJvcnZhbFwiO1xuXG4vLyBDYWNoZSBmb3IgT3BlbkFQSSBzcGVjIGhhc2hlcyB0byBkZXRlY3QgY2hhbmdlc1xuY29uc3Qgc3BlY0hhc2hDYWNoZSA9IG5ldyBNYXA8c3RyaW5nLCBzdHJpbmc+KCk7XG5cbmV4cG9ydCB0eXBlIFN0ZXBBY3Rpb24gPSBzdHJpbmcgfCAoKCkgPT4gdm9pZCB8IFByb21pc2U8dm9pZD4pO1xuXG5leHBvcnQgdHlwZSBTdGVwU3BlYyA9IHtcbiAgbmFtZTogc3RyaW5nO1xuICBhY3Rpb246IFN0ZXBBY3Rpb247XG59O1xuXG5leHBvcnQgY29uc3QgU3RlcCA9IChzcGVjOiBTdGVwU3BlYyk6IFN0ZXBTcGVjID0+IHNwZWM7XG5cbi8qKlxuICogUHJlZGVmaW5lZCBzdGVwIGZvciBnZW5lcmF0aW5nIE9wZW5BUEkgc2NoZW1hXG4gKiBAcGFyYW0gYXBwTW9kdWxlIC0gVGhlIFB5dGhvbiBtb2R1bGUgcGF0aCAoZS5nLiwgXCJzYW1wbGUuYXBpLmFwcDphcHBcIilcbiAqIEBwYXJhbSBvdXRwdXRQYXRoIC0gV2hlcmUgdG8gd3JpdGUgdGhlIE9wZW5BUEkgSlNPTiBmaWxlXG4gKi9cbmV4cG9ydCBjb25zdCBPcGVuQVBJID0gKGFwcE1vZHVsZTogc3RyaW5nLCBvdXRwdXRQYXRoOiBzdHJpbmcpOiBTdGVwU3BlYyA9PiAoe1xuICBuYW1lOiBcIm9wZW5hcGlcIixcbiAgYWN0aW9uOiBgdXYgcnVuIGFweCBvcGVuYXBpICR7YXBwTW9kdWxlfSAke291dHB1dFBhdGh9YCxcbn0pO1xuXG4vKipcbiAqIFByZWRlZmluZWQgc3RlcCBmb3IgZ2VuZXJhdGluZyBBUEkgY2xpZW50IHdpdGggT3J2YWxcbiAqIFNraXBzIGdlbmVyYXRpb24gaWYgdGhlIE9wZW5BUEkgc3BlYyBoYXNuJ3QgY2hhbmdlZCBzaW5jZSBsYXN0IHJ1blxuICogQHBhcmFtIGlucHV0IC0gUGF0aCB0byB0aGUgT3BlbkFQSSBzcGVjIGZpbGVcbiAqIEBwYXJhbSBvdXRwdXQgLSBPcnZhbCBvdXRwdXQgY29uZmlndXJhdGlvblxuICovXG5leHBvcnQgY29uc3QgT3J2YWwgPSAoe1xuICBpbnB1dCxcbiAgb3V0cHV0LFxufToge1xuICBpbnB1dDogc3RyaW5nO1xuICBvdXRwdXQ6IE91dHB1dE9wdGlvbnM7XG59KTogU3RlcFNwZWMgPT4gKHtcbiAgbmFtZTogXCJvcnZhbFwiLFxuICBhY3Rpb246IGFzeW5jICgpID0+IHtcbiAgICAvLyBDaGVjayBpZiBzcGVjIGZpbGUgZXhpc3RzXG4gICAgaWYgKCFleGlzdHNTeW5jKGlucHV0KSkge1xuICAgICAgY29uc29sZS53YXJuKFxuICAgICAgICBgW2FweF0gT3BlbkFQSSBzcGVjIG5vdCBmb3VuZCBhdCAke2lucHV0fSwgc2tpcHBpbmcgT3J2YWwgZ2VuZXJhdGlvbmAsXG4gICAgICApO1xuICAgICAgcmV0dXJuO1xuICAgIH1cblxuICAgIC8vIFJlYWQgYW5kIGhhc2ggdGhlIHNwZWMgZmlsZVxuICAgIGNvbnN0IHNwZWNDb250ZW50ID0gcmVhZEZpbGVTeW5jKGlucHV0LCBcInV0Zi04XCIpO1xuICAgIGNvbnN0IHNwZWNIYXNoID0gY3JlYXRlSGFzaChcInNoYTI1NlwiKS51cGRhdGUoc3BlY0NvbnRlbnQpLmRpZ2VzdChcImhleFwiKTtcblxuICAgIC8vIENoZWNrIGlmIHNwZWMgaGFzIGNoYW5nZWRcbiAgICBjb25zdCBjYWNoZWRIYXNoID0gc3BlY0hhc2hDYWNoZS5nZXQoaW5wdXQpO1xuICAgIGlmIChjYWNoZWRIYXNoID09PSBzcGVjSGFzaCkge1xuICAgICAgY29uc29sZS5sb2coYFthcHhdIE9wZW5BUEkgc3BlYyB1bmNoYW5nZWQsIHNraXBwaW5nIE9ydmFsIGdlbmVyYXRpb25gKTtcbiAgICAgIHJldHVybjtcbiAgICB9XG5cbiAgICAvLyBHZW5lcmF0ZSBBUEkgY2xpZW50XG4gICAgYXdhaXQgZ2VuZXJhdGUoe1xuICAgICAgaW5wdXQsXG4gICAgICBvdXRwdXQsXG4gICAgfSk7XG5cbiAgICAvLyBVcGRhdGUgY2FjaGVcbiAgICBzcGVjSGFzaENhY2hlLnNldChpbnB1dCwgc3BlY0hhc2gpO1xuICB9LFxufSk7XG5cbmV4cG9ydCBpbnRlcmZhY2UgQXB4UGx1Z2luT3B0aW9ucyB7XG4gIHN0ZXBzPzogU3RlcFNwZWNbXTtcbiAgaWdub3JlPzogc3RyaW5nW107XG59XG5cbmV4cG9ydCBmdW5jdGlvbiBhcHgob3B0aW9uczogQXB4UGx1Z2luT3B0aW9ucyA9IHt9KTogUGx1Z2luIHtcbiAgY29uc3QgeyBzdGVwcyA9IFtdLCBpZ25vcmUgPSBbXSB9ID0gb3B0aW9ucztcblxuICBsZXQgb3V0RGlyOiBzdHJpbmc7XG4gIGxldCB0aW1lcjogTm9kZUpTLlRpbWVvdXQgfCBudWxsID0gbnVsbDtcbiAgbGV0IHN0b3BwaW5nID0gZmFsc2U7XG4gIGxldCByZXNvbHZlZElnbm9yZXM6IHN0cmluZ1tdID0gW107XG4gIGxldCBpc1J1bm5pbmdTdGVwcyA9IGZhbHNlO1xuICBsZXQgY2hpbGRQcm9jZXNzZXM6IENoaWxkUHJvY2Vzc1tdID0gW107XG5cbiAgLyoqXG4gICAqIEV4ZWN1dGVzIGEgc2hlbGwgY29tbWFuZCB1c2luZyBzcGF3biwgd2l0aCBwcm9wZXIgc2lnbmFsIGhhbmRsaW5nXG4gICAqL1xuICBmdW5jdGlvbiBleGVjdXRlU2hlbGxDb21tYW5kKGNvbW1hbmQ6IHN0cmluZyk6IFByb21pc2U8dm9pZD4ge1xuICAgIHJldHVybiBuZXcgUHJvbWlzZSgocmVzb2x2ZSwgcmVqZWN0KSA9PiB7XG4gICAgICBpZiAoc3RvcHBpbmcpIHtcbiAgICAgICAgY29uc29sZS5sb2coYFthcHhdIFNraXBwaW5nIGNvbW1hbmQgKHN0b3BwaW5nKTogJHtjb21tYW5kfWApO1xuICAgICAgICByZXNvbHZlKCk7XG4gICAgICAgIHJldHVybjtcbiAgICAgIH1cblxuICAgICAgY29uc29sZS5sb2coYFthcHhdIEV4ZWN1dGluZzogJHtjb21tYW5kfWApO1xuXG4gICAgICAvLyBQYXJzZSBjb21tYW5kIGludG8gY29tbWFuZCBhbmQgYXJnc1xuICAgICAgY29uc3QgcGFydHMgPSBjb21tYW5kLnNwbGl0KC9cXHMrLyk7XG4gICAgICBjb25zdCBjbWQgPSBwYXJ0c1swXTtcbiAgICAgIGNvbnN0IGFyZ3MgPSBwYXJ0cy5zbGljZSgxKTtcblxuICAgICAgLy8gU3Bhd24gcHJvY2VzcyB3aXRoIHByb3BlciBzaWduYWwgaGFuZGxpbmdcbiAgICAgIGNvbnN0IGNoaWxkID0gc3Bhd24oY21kLCBhcmdzLCB7XG4gICAgICAgIHN0ZGlvOiBcImluaGVyaXRcIiwgLy8gRm9yd2FyZCBzdGRvdXQvc3RkZXJyIHRvIHBhcmVudFxuICAgICAgICBzaGVsbDogdHJ1ZSwgLy8gVXNlIHNoZWxsIGZvciBwcm9wZXIgY29tbWFuZCBwYXJzaW5nXG4gICAgICAgIGRldGFjaGVkOiBmYWxzZSwgLy8gS2VlcCBpbiBzYW1lIHByb2Nlc3MgZ3JvdXAgZm9yIHNpZ25hbCBwcm9wYWdhdGlvblxuICAgICAgfSk7XG5cbiAgICAgIC8vIFRyYWNrIGNoaWxkIHByb2Nlc3MgZm9yIGNsZWFudXBcbiAgICAgIGNoaWxkUHJvY2Vzc2VzLnB1c2goY2hpbGQpO1xuICAgICAgY29uc29sZS5sb2coYFthcHhdIFN0YXJ0ZWQgcHJvY2VzcyBQSUQ6ICR7Y2hpbGQucGlkfWApO1xuXG4gICAgICBjaGlsZC5vbihcImVycm9yXCIsIChlcnIpID0+IHtcbiAgICAgICAgY29uc29sZS5lcnJvcihgW2FweF0gUHJvY2VzcyBlcnJvcjpgLCBlcnIpO1xuICAgICAgICByZWplY3QoZXJyKTtcbiAgICAgIH0pO1xuXG4gICAgICBjaGlsZC5vbihcImV4aXRcIiwgKGNvZGUsIHNpZ25hbCkgPT4ge1xuICAgICAgICAvLyBSZW1vdmUgZnJvbSB0cmFja2luZ1xuICAgICAgICBjaGlsZFByb2Nlc3NlcyA9IGNoaWxkUHJvY2Vzc2VzLmZpbHRlcigocCkgPT4gcC5waWQgIT09IGNoaWxkLnBpZCk7XG5cbiAgICAgICAgaWYgKHNpZ25hbCkge1xuICAgICAgICAgIGNvbnNvbGUubG9nKFxuICAgICAgICAgICAgYFthcHhdIFByb2Nlc3MgJHtjaGlsZC5waWR9IGV4aXRlZCB3aXRoIHNpZ25hbDogJHtzaWduYWx9YCxcbiAgICAgICAgICApO1xuICAgICAgICAgIHJlc29sdmUoKTsgLy8gVHJlYXQgc2lnbmFsIHRlcm1pbmF0aW9uIGFzIHN1Y2Nlc3MgZm9yIGNsZWFudXAgc2NlbmFyaW9zXG4gICAgICAgIH0gZWxzZSBpZiAoY29kZSAhPT0gMCkge1xuICAgICAgICAgIGNvbnNvbGUuZXJyb3IoYFthcHhdIFByb2Nlc3MgJHtjaGlsZC5waWR9IGV4aXRlZCB3aXRoIGNvZGU6ICR7Y29kZX1gKTtcbiAgICAgICAgICByZWplY3QobmV3IEVycm9yKGBDb21tYW5kIGZhaWxlZCB3aXRoIGV4aXQgY29kZSAke2NvZGV9YCkpO1xuICAgICAgICB9IGVsc2Uge1xuICAgICAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSBQcm9jZXNzICR7Y2hpbGQucGlkfSBjb21wbGV0ZWQgc3VjY2Vzc2Z1bGx5YCk7XG4gICAgICAgICAgcmVzb2x2ZSgpO1xuICAgICAgICB9XG4gICAgICB9KTtcblxuICAgICAgLy8gSWYgd2UncmUgc3RvcHBpbmcsIGtpbGwgdGhlIHByb2Nlc3MgaW1tZWRpYXRlbHlcbiAgICAgIGlmIChzdG9wcGluZyAmJiBjaGlsZC5waWQpIHtcbiAgICAgICAgY29uc29sZS5sb2coYFthcHhdIEtpbGxpbmcgcHJvY2VzcyAke2NoaWxkLnBpZH0gKHN0b3BwaW5nKWApO1xuICAgICAgICBraWxsUHJvY2VzcyhjaGlsZCk7XG4gICAgICB9XG4gICAgfSk7XG4gIH1cblxuICAvKipcbiAgICogS2lsbHMgYSBwcm9jZXNzIGFuZCBhbGwgaXRzIGNoaWxkcmVuXG4gICAqL1xuICBmdW5jdGlvbiBraWxsUHJvY2Vzcyhwcm9jOiBDaGlsZFByb2Nlc3MpOiB2b2lkIHtcbiAgICBpZiAoIXByb2MucGlkKSByZXR1cm47XG5cbiAgICB0cnkge1xuICAgICAgLy8gT24gVW5peC1saWtlIHN5c3RlbXMsIGtpbGwgdGhlIHByb2Nlc3MgZ3JvdXBcbiAgICAgIC8vIE5lZ2F0aXZlIFBJRCBraWxscyB0aGUgZW50aXJlIHByb2Nlc3MgZ3JvdXBcbiAgICAgIGlmIChwcm9jZXNzLnBsYXRmb3JtICE9PSBcIndpbjMyXCIpIHtcbiAgICAgICAgcHJvY2Vzcy5raWxsKC1wcm9jLnBpZCwgXCJTSUdURVJNXCIpO1xuICAgICAgICBjb25zb2xlLmxvZyhgW2FweF0gU2VudCBTSUdURVJNIHRvIHByb2Nlc3MgZ3JvdXAgLSR7cHJvYy5waWR9YCk7XG4gICAgICB9IGVsc2Uge1xuICAgICAgICAvLyBPbiBXaW5kb3dzLCBqdXN0IGtpbGwgdGhlIHByb2Nlc3NcbiAgICAgICAgcHJvYy5raWxsKFwiU0lHVEVSTVwiKTtcbiAgICAgICAgY29uc29sZS5sb2coYFthcHhdIFNlbnQgU0lHVEVSTSB0byBwcm9jZXNzICR7cHJvYy5waWR9YCk7XG4gICAgICB9XG4gICAgfSBjYXRjaCAoZXJyKSB7XG4gICAgICBjb25zb2xlLmVycm9yKGBbYXB4XSBFcnJvciBraWxsaW5nIHByb2Nlc3MgJHtwcm9jLnBpZH06YCwgZXJyKTtcbiAgICAgIC8vIFRyeSBmb3JjZWZ1bCBraWxsIGFzIGZhbGxiYWNrXG4gICAgICB0cnkge1xuICAgICAgICBwcm9jLmtpbGwoXCJTSUdLSUxMXCIpO1xuICAgICAgfSBjYXRjaCAoZSkge1xuICAgICAgICAvLyBJZ25vcmUgZXJyb3JzIG9uIGZvcmNlZnVsIGtpbGxcbiAgICAgIH1cbiAgICB9XG4gIH1cblxuICBhc3luYyBmdW5jdGlvbiBleGVjdXRlQWN0aW9uKGFjdGlvbjogU3RlcEFjdGlvbik6IFByb21pc2U8dm9pZD4ge1xuICAgIGlmIChzdG9wcGluZykge1xuICAgICAgY29uc29sZS5sb2coYFthcHhdIFNraXBwaW5nIGFjdGlvbiAoc3RvcHBpbmcpYCk7XG4gICAgICByZXR1cm47XG4gICAgfVxuXG4gICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgaWYgKHR5cGVvZiBhY3Rpb24gPT09IFwic3RyaW5nXCIpIHtcbiAgICAgIC8vIEV4ZWN1dGUgYXMgc2hlbGwgY29tbWFuZFxuICAgICAgYXdhaXQgZXhlY3V0ZVNoZWxsQ29tbWFuZChhY3Rpb24pO1xuICAgIH0gZWxzZSB7XG4gICAgICAvLyBFeGVjdXRlIGFzIGZ1bmN0aW9uXG4gICAgICBpZiAoc3RvcHBpbmcpIHJldHVybjtcbiAgICAgIGF3YWl0IGFjdGlvbigpO1xuICAgIH1cbiAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcbiAgfVxuXG4gIGFzeW5jIGZ1bmN0aW9uIHJ1bkFsbFN0ZXBzKCk6IFByb21pc2U8dm9pZD4ge1xuICAgIGlmIChzdG9wcGluZykge1xuICAgICAgY29uc29sZS5sb2coYFthcHhdIFNraXBwaW5nIHN0ZXBzIChzdG9wcGluZylgKTtcbiAgICAgIHJldHVybjtcbiAgICB9XG5cbiAgICBpZiAoaXNSdW5uaW5nU3RlcHMpIHtcbiAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSBTdGVwcyBhbHJlYWR5IHJ1bm5pbmcsIHNraXBwaW5nLi4uYCk7XG4gICAgICByZXR1cm47XG4gICAgfVxuXG4gICAgY29uc29sZS5sb2coYFthcHhdIFJ1bm5pbmcgJHtzdGVwcy5sZW5ndGh9IHN0ZXAocykuLi5gKTtcbiAgICBpc1J1bm5pbmdTdGVwcyA9IHRydWU7XG5cbiAgICB0cnkge1xuICAgICAgZm9yIChjb25zdCBzdGVwIG9mIHN0ZXBzKSB7XG4gICAgICAgIGlmIChzdG9wcGluZykge1xuICAgICAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSBTdG9wcGluZyBkdXJpbmcgc3RlcCBleGVjdXRpb25gKTtcbiAgICAgICAgICBicmVhaztcbiAgICAgICAgfVxuICAgICAgICBjb25zdCBzdGFydCA9IERhdGUubm93KCk7XG4gICAgICAgIHRyeSB7XG4gICAgICAgICAgY29uc29sZS5sb2coYFthcHhdICR7c3RlcC5uYW1lfSDij7NgKTtcbiAgICAgICAgICBhd2FpdCBleGVjdXRlQWN0aW9uKHN0ZXAuYWN0aW9uKTtcbiAgICAgICAgICBjb25zb2xlLmxvZyhgW2FweF0gJHtzdGVwLm5hbWV9IOKckyAoJHtEYXRlLm5vdygpIC0gc3RhcnR9IG1zKWApO1xuICAgICAgICB9IGNhdGNoIChlcnIpIHtcbiAgICAgICAgICBjb25zb2xlLmVycm9yKGBbYXB4XSAke3N0ZXAubmFtZX0g4pyXYCwgZXJyKTtcbiAgICAgICAgICB0aHJvdyBlcnI7XG4gICAgICAgIH1cbiAgICAgIH1cbiAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSBBbGwgc3RlcHMgY29tcGxldGVkYCk7XG4gICAgfSBmaW5hbGx5IHtcbiAgICAgIGlzUnVubmluZ1N0ZXBzID0gZmFsc2U7XG4gICAgfVxuICB9XG5cbiAgLyoqXG4gICAqIEVuc3VyZXMgdGhlIG91dHB1dCBkaXJlY3RvcnkgZXhpc3RzIGFuZCBjb250YWlucyBhIC5naXRpZ25vcmUgZmlsZS5cbiAgICogVGhpcyBpcyBjYWxsZWQgYXQgbXVsdGlwbGUgcG9pbnRzIHRvIGd1YXJhbnRlZSB0aGUgZGlyZWN0b3J5IGlzIGFsd2F5cyBwcmVzZW50LlxuICAgKi9cbiAgZnVuY3Rpb24gZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk6IHZvaWQge1xuICAgIGlmICghb3V0RGlyKSB7XG4gICAgICBjb25zb2xlLmVycm9yKGBbYXB4XSBvdXREaXIgaXMgbm90IHNldGApO1xuICAgICAgcmV0dXJuO1xuICAgIH1cblxuICAgIHRyeSB7XG4gICAgICAvLyBBbHdheXMgZW5zdXJlIHRoZSBvdXRwdXQgZGlyZWN0b3J5IGV4aXN0c1xuICAgICAgaWYgKCFleGlzdHNTeW5jKG91dERpcikpIHtcbiAgICAgICAgbWtkaXJTeW5jKG91dERpciwgeyByZWN1cnNpdmU6IHRydWUgfSk7XG4gICAgICB9XG5cbiAgICAgIC8vIEFsd2F5cyBlbnN1cmUgLmdpdGlnbm9yZSBleGlzdHMgaW4gb3V0cHV0IGRpcmVjdG9yeVxuICAgICAgY29uc3QgZ2l0aWdub3JlUGF0aCA9IGpvaW4ob3V0RGlyLCBcIi5naXRpZ25vcmVcIik7XG4gICAgICBpZiAoIWV4aXN0c1N5bmMoZ2l0aWdub3JlUGF0aCkpIHtcbiAgICAgICAgd3JpdGVGaWxlU3luYyhnaXRpZ25vcmVQYXRoLCBcIipcXG5cIik7XG4gICAgICB9XG4gICAgfSBjYXRjaCAoZXJyKSB7XG4gICAgICBjb25zb2xlLmVycm9yKGBbYXB4XSBmYWlsZWQgdG8gZW5zdXJlIG91dHB1dCBkaXJlY3Rvcnk6YCwgZXJyKTtcbiAgICB9XG4gIH1cblxuICBmdW5jdGlvbiBzdG9wKCk6IHZvaWQge1xuICAgIGlmIChzdG9wcGluZykgcmV0dXJuO1xuICAgIGNvbnNvbGUubG9nKGBbYXB4XSBTdG9wcGluZy4uLiAoJHtjaGlsZFByb2Nlc3Nlcy5sZW5ndGh9IGNoaWxkIHByb2Nlc3NlcylgKTtcbiAgICBzdG9wcGluZyA9IHRydWU7XG5cbiAgICAvLyBDbGVhciBhbnkgcGVuZGluZyB0aW1lcnNcbiAgICBpZiAodGltZXIpIHtcbiAgICAgIGNsZWFyVGltZW91dCh0aW1lcik7XG4gICAgICB0aW1lciA9IG51bGw7XG4gICAgfVxuXG4gICAgLy8gS2lsbCBhbGwgdHJhY2tlZCBjaGlsZCBwcm9jZXNzZXNcbiAgICBpZiAoY2hpbGRQcm9jZXNzZXMubGVuZ3RoID4gMCkge1xuICAgICAgY29uc29sZS5sb2coXG4gICAgICAgIGBbYXB4XSBLaWxsaW5nICR7Y2hpbGRQcm9jZXNzZXMubGVuZ3RofSBjaGlsZCBwcm9jZXNzKGVzKS4uLmAsXG4gICAgICApO1xuICAgICAgY2hpbGRQcm9jZXNzZXMuZm9yRWFjaCgocHJvYykgPT4ge1xuICAgICAgICBpZiAocHJvYy5waWQpIHtcbiAgICAgICAgICBraWxsUHJvY2Vzcyhwcm9jKTtcbiAgICAgICAgfVxuICAgICAgfSk7XG4gICAgICBjaGlsZFByb2Nlc3NlcyA9IFtdO1xuICAgIH1cblxuICAgIGNvbnNvbGUubG9nKGBbYXB4XSBTdG9wcGVkYCk7XG4gIH1cblxuICBmdW5jdGlvbiByZXNldCgpOiB2b2lkIHtcbiAgICBjb25zb2xlLmxvZyhgW2FweF0gUmVzZXR0aW5nIHBsdWdpbiBzdGF0ZWApO1xuICAgIHN0b3BwaW5nID0gZmFsc2U7XG4gICAgdGltZXIgPSBudWxsO1xuICAgIGlzUnVubmluZ1N0ZXBzID0gZmFsc2U7XG4gICAgY2hpbGRQcm9jZXNzZXMgPSBbXTtcbiAgfVxuXG4gIHJldHVybiB7XG4gICAgbmFtZTogXCJhcHhcIixcbiAgICBhcHBseTogKCkgPT4gdHJ1ZSxcblxuICAgIGNvbmZpZ1Jlc29sdmVkKGNvbmZpZykge1xuICAgICAgb3V0RGlyID0gcmVzb2x2ZShjb25maWcucm9vdCwgY29uZmlnLmJ1aWxkLm91dERpcik7XG4gICAgICByZXNvbHZlZElnbm9yZXMgPSBpZ25vcmUubWFwKChwYXR0ZXJuKSA9PlxuICAgICAgICByZXNvbHZlKHByb2Nlc3MuY3dkKCksIHBhdHRlcm4pLFxuICAgICAgKTtcblxuICAgICAgLy8gUmVzZXQgc3RhdGUgZm9yIG5ldyBidWlsZFxuICAgICAgcmVzZXQoKTtcblxuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgYXMgc29vbiBhcyB3ZSBrbm93IHRoZSBvdXREaXJcbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgIH0sXG5cbiAgICBjb25maWd1cmVTZXJ2ZXIoc2VydmVyKSB7XG4gICAgICAvLyBMZXQgVml0ZSBoYW5kbGUgU0lHSU5UL1NJR1RFUk0gLSB3ZSdsbCBjbGVhbiB1cCB2aWEgc2VydmVyLmNsb3NlIGFuZCBjbG9zZUJ1bmRsZVxuICAgICAgLy8gRE9OJ1QgYWRkIHNpZ25hbCBoYW5kbGVycyBoZXJlIGFzIHRoZXkgaW50ZXJmZXJlIHdpdGggVml0ZSdzIHNpZ25hbCBoYW5kbGluZ1xuICAgICAgLy8gU2VlOiBodHRwczovL2dpdGh1Yi5jb20vdml0ZWpzL3ZpdGUvaXNzdWVzLzExNDM0XG4gICAgICBzZXJ2ZXIuaHR0cFNlcnZlcj8ub25jZShcImNsb3NlXCIsICgpID0+IHtcbiAgICAgICAgY29uc29sZS5sb2coYFthcHhdIFNlcnZlciBjbG9zaW5nLCBzdG9wcGluZyBwbHVnaW4uLi5gKTtcbiAgICAgICAgc3RvcCgpO1xuICAgICAgfSk7XG5cbiAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIHdoZW4gc2VydmVyIHN0YXJ0c1xuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgfSxcblxuICAgIGFzeW5jIGJ1aWxkU3RhcnQoKSB7XG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBiZWZvcmUgYnVpbGQgc3RhcnRzXG4gICAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcblxuICAgICAgaWYgKHN0ZXBzLmxlbmd0aCA+IDApIHtcbiAgICAgICAgYXdhaXQgcnVuQWxsU3RlcHMoKTtcbiAgICAgIH1cbiAgICB9LFxuXG4gICAgaGFuZGxlSG90VXBkYXRlKGN0eCkge1xuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgb24gZXZlcnkgSE1SIHVwZGF0ZVxuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG5cbiAgICAgIC8vIERvbid0IHRyaWdnZXIgdXBkYXRlcyBpZiBzdG9wcGluZ1xuICAgICAgaWYgKHN0b3BwaW5nKSB7XG4gICAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSBITVIgdXBkYXRlIGlnbm9yZWQgKHN0b3BwaW5nKWApO1xuICAgICAgICByZXR1cm47XG4gICAgICB9XG5cbiAgICAgIC8vIENoZWNrIGlmIGZpbGUgc2hvdWxkIGJlIGlnbm9yZWRcbiAgICAgIGlmIChyZXNvbHZlZElnbm9yZXMuc29tZSgocGF0dGVybikgPT4gY3R4LmZpbGUuaW5jbHVkZXMocGF0dGVybikpKSB7XG4gICAgICAgIGNvbnNvbGUubG9nKFxuICAgICAgICAgIGBbYXB4XSBITVIgdXBkYXRlIGlnbm9yZWQgKG1hdGNoZXMgaWdub3JlIHBhdHRlcm4pOiAke2N0eC5maWxlfWAsXG4gICAgICAgICk7XG4gICAgICAgIHJldHVybjtcbiAgICAgIH1cblxuICAgICAgY29uc29sZS5sb2coYFthcHhdIEhNUiB1cGRhdGUgZGV0ZWN0ZWQ6ICR7Y3R4LmZpbGV9YCk7XG5cbiAgICAgIC8vIERlYm91bmNlIHN0ZXAgZXhlY3V0aW9uIG9uIEhNUiB1cGRhdGVzXG4gICAgICBpZiAodGltZXIpIHtcbiAgICAgICAgY2xlYXJUaW1lb3V0KHRpbWVyKTtcbiAgICAgICAgY29uc29sZS5sb2coYFthcHhdIEhNUiBkZWJvdW5jZWQgKHJlc2V0dGluZyB0aW1lcilgKTtcbiAgICAgIH1cblxuICAgICAgdGltZXIgPSBzZXRUaW1lb3V0KGFzeW5jICgpID0+IHtcbiAgICAgICAgdGltZXIgPSBudWxsO1xuXG4gICAgICAgIC8vIERvdWJsZS1jaGVjayB3ZSdyZSBub3Qgc3RvcHBpbmcgYmVmb3JlIHJ1bm5pbmdcbiAgICAgICAgaWYgKHN0b3BwaW5nKSB7XG4gICAgICAgICAgY29uc29sZS5sb2coYFthcHhdIEhNUiBjYWxsYmFjayBjYW5jZWxsZWQgKHN0b3BwaW5nKWApO1xuICAgICAgICAgIHJldHVybjtcbiAgICAgICAgfVxuXG4gICAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSBITVIgdHJpZ2dlcmVkIHN0ZXAgZXhlY3V0aW9uYCk7XG4gICAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIGJlZm9yZSBydW5uaW5nIHN0ZXBzXG4gICAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgICAgICBhd2FpdCBydW5BbGxTdGVwcygpO1xuXG4gICAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIGFmdGVyIHJ1bm5pbmcgc3RlcHNcbiAgICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgICB9LCAxMDApO1xuXG4gICAgICAvLyBBbGxvdyB0aGUgcHJvY2VzcyB0byBleGl0IGV2ZW4gaWYgdGhpcyB0aW1lciBpcyBwZW5kaW5nXG4gICAgICB0aW1lci51bnJlZigpO1xuICAgIH0sXG5cbiAgICB3cml0ZUJ1bmRsZSgpIHtcbiAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIGFmdGVyIGZpbGVzIGFyZSB3cml0dGVuXG4gICAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcbiAgICB9LFxuXG4gICAgY2xvc2VCdW5kbGUoKSB7XG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBvbmUgZmluYWwgdGltZVxuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgICBzdG9wKCk7XG4gICAgfSxcbiAgfTtcbn1cblxuLy8gRGVmYXVsdCBleHBvcnQgZm9yIGNvbnZlbmllbmNlOiBpbXBvcnQgYXB4IGZyb20gXCJhcHhcIlxuZXhwb3J0IGRlZmF1bHQgYXB4O1xuIgogIF0sCiAgIm1hcHBpbmdzIjogIjtBQUFBO0FBQ0E7QUFFQTtBQUNBO0FBQ0E7QUEwRU8sU0FBUyxHQUFHLENBQUMsVUFBNEIsQ0FBQyxHQUFXO0FBQzFELFVBQVEsUUFBUSxDQUFDLEdBQUcsU0FBUyxDQUFDLE1BQU07QUFFcEMsTUFBSTtBQUNKLE1BQUksUUFBK0I7QUFDbkMsTUFBSSxXQUFXO0FBQ2YsTUFBSSxrQkFBNEIsQ0FBQztBQUNqQyxNQUFJ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QUFvQztBQUVoRCxpQ0FBeUI7QUFDekIsY0FBTSxZQUFZO0FBR2xCLGlDQUF5QjtBQUFBLFNBQ3hCLEdBQUc7QUFHTixZQUFNLE1BQU07QUFBQTtBQUFBLElBR2QsV0FBVyxHQUFHO0FBRVosK0JBQXlCO0FBQUE7QUFBQSxJQUczQixXQUFXLEdBQUc7QUFFWiwrQkFBeUI7QUFDekIsV0FBSztBQUFBO0FBQUEsRUFFVDtBQUFBO0FBN1hGLElBQU0sZ0JBQWdCLElBQUk7QUFTbkIsSUFBTSxPQUFPLENBQUMsU0FBNkI7QUFPM0MsSUFBTSxVQUFVLENBQUMsV0FBbUIsZ0JBQWtDO0FBQUEsRUFDM0UsTUFBTTtBQUFBLEVBQ04sUUFBUSxzQkFBc0IsYUFBYTtBQUM3QztBQVFPLElBQU0sUUFBUTtBQUFBLEVBQ25CO0FBQUEsRUFDQTtBQUFBLE9BSWU7QUFBQSxFQUNmLE1BQU07QUFBQSxFQUNOLFFBQVEsWUFBWTtBQUVsQixTQUFLLFdBQVcsS0FBSyxHQUFHO0FBQ3RCLGNBQVEsS0FDTixtQ0FBbUMsa0NBQ3JDO0FBQ0E7QUFBQSxJQUNGO0FBR0EsVUFBTSxjQUFjLGFBQWEsT0FBTyxPQUFPO0FBQy9DLFVBQU0sV0FBVyxXQUFXLFFBQVEsRUFBRSxPQUFPLFdBQVcsRUFBRSxPQUFPLEtBQUs7QUFHdEUsVUFBTSxhQUFhLGNBQWMsSUFBSSxLQUFLO0FBQzFDLFFBQUksZUFBZSxVQUFVO0FBQzNCLGNBQVEsSUFBSSx5REFBeUQ7QUFDckU7QUFBQSxJQUNGO0FBR0EsVUFBTSxTQUFTO0FBQUEsTUFDYjtBQUFBLE1BQ0E7QUFBQSxJQUNGLENBQUM7QUFHRCxrQkFBYyxJQUFJLE9BQU8sUUFBUTtBQUFBO0FBRXJDO0FBaVVBLElBQWU7IiwKICAiZGVidWdJZCI6ICI4MEMxREU4RkJGRTBFOTY2NjQ3NTZFMjE2NDc1NkUyMSIsCiAgIm5hbWVzIjogW10KfQ==
