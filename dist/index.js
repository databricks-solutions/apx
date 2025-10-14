// src/apx/plugins/index.ts
import { existsSync, mkdirSync, writeFileSync } from "fs";
import { join, resolve } from "path";
import { exec } from "child_process";
import { promisify } from "util";
function apx(options = {}) {
  const { steps = [], ignore = [] } = options;
  let outDir;
  let timer = null;
  let stopping = false;
  let resolvedIgnores = [];
  async function executeAction(action) {
    if (typeof action === "string") {
      const { stdout, stderr } = await execAsync(action);
      if (stdout) console.log(stdout.trim());
      if (stderr) console.error(stderr.trim());
    } else {
      await action();
    }
  }
  async function runAllSteps() {
    for (const step of steps) {
      if (stopping) break;
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
  }
  function ensureGitignoreInOutDir() {
    if (!outDir) return;
    if (!existsSync(outDir)) {
      mkdirSync(outDir, { recursive: true });
    }
    const gitignorePath = join(outDir, ".gitignore");
    if (!existsSync(gitignorePath)) {
      writeFileSync(gitignorePath, "*\n");
      console.log(`[apx] ensured ${gitignorePath}`);
    }
  }
  function stop() {
    if (stopping) return;
    stopping = true;
    if (timer) clearTimeout(timer);
    console.log("[apx] stopping...");
  }
  return {
    name: "apx",
    apply: () => true,
    configResolved(config) {
      outDir = config.build.outDir;
      resolvedIgnores = ignore.map((pattern) =>
        resolve(process.cwd(), pattern),
      );
      process.on("SIGINT", stop);
      process.on("SIGTERM", stop);
    },
    configureServer(server) {
      server.httpServer?.once("close", stop);
    },
    async buildStart() {
      ensureGitignoreInOutDir();
      if (steps.length > 0) {
        await runAllSteps();
      }
    },
    handleHotUpdate(ctx) {
      if (resolvedIgnores.some((pattern) => ctx.file.includes(pattern))) {
        return;
      }
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => {
        timer = null;
        runAllSteps();
      }, 100);
    },
    closeBundle() {
      ensureGitignoreInOutDir();
      stop();
    },
  };
}
var execAsync = promisify(exec);
var Step = (spec) => spec;
var plugins_default = apx;
export { plugins_default as default, apx, Step };

//# debugId=EA364735EFE10EC464756E2164756E21
//# sourceMappingURL=data:application/json;base64,ewogICJ2ZXJzaW9uIjogMywKICAic291cmNlcyI6IFsic3JjL2FweC9wbHVnaW5zL2luZGV4LnRzIl0sCiAgInNvdXJjZXNDb250ZW50IjogWwogICAgImltcG9ydCB7IGV4aXN0c1N5bmMsIG1rZGlyU3luYywgd3JpdGVGaWxlU3luYyB9IGZyb20gXCJmc1wiO1xuaW1wb3J0IHsgam9pbiwgcmVzb2x2ZSB9IGZyb20gXCJwYXRoXCI7XG5pbXBvcnQgeyB0eXBlIFBsdWdpbiB9IGZyb20gXCJ2aXRlXCI7XG5pbXBvcnQgeyBleGVjIH0gZnJvbSBcImNoaWxkX3Byb2Nlc3NcIjtcbmltcG9ydCB7IHByb21pc2lmeSB9IGZyb20gXCJ1dGlsXCI7XG5cbmNvbnN0IGV4ZWNBc3luYyA9IHByb21pc2lmeShleGVjKTtcblxuZXhwb3J0IHR5cGUgU3RlcEFjdGlvbiA9IHN0cmluZyB8ICgoKSA9PiB2b2lkIHwgUHJvbWlzZTx2b2lkPik7XG5cbmV4cG9ydCB0eXBlIFN0ZXBTcGVjID0ge1xuICBuYW1lOiBzdHJpbmc7XG4gIGFjdGlvbjogU3RlcEFjdGlvbjtcbn07XG5cbmV4cG9ydCBjb25zdCBTdGVwID0gKHNwZWM6IFN0ZXBTcGVjKTogU3RlcFNwZWMgPT4gc3BlYztcblxuZXhwb3J0IGludGVyZmFjZSBBcHhQbHVnaW5PcHRpb25zIHtcbiAgc3RlcHM/OiBTdGVwU3BlY1tdO1xuICBpZ25vcmU/OiBzdHJpbmdbXTtcbn1cblxuZXhwb3J0IGZ1bmN0aW9uIGFweChvcHRpb25zOiBBcHhQbHVnaW5PcHRpb25zID0ge30pOiBQbHVnaW4ge1xuICBjb25zdCB7IHN0ZXBzID0gW10sIGlnbm9yZSA9IFtdIH0gPSBvcHRpb25zO1xuICBcbiAgbGV0IG91dERpcjogc3RyaW5nO1xuICBsZXQgdGltZXI6IE5vZGVKUy5UaW1lb3V0IHwgbnVsbCA9IG51bGw7XG4gIGxldCBzdG9wcGluZyA9IGZhbHNlO1xuICBsZXQgcmVzb2x2ZWRJZ25vcmVzOiBzdHJpbmdbXSA9IFtdO1xuXG4gIGFzeW5jIGZ1bmN0aW9uIGV4ZWN1dGVBY3Rpb24oYWN0aW9uOiBTdGVwQWN0aW9uKTogUHJvbWlzZTx2b2lkPiB7XG4gICAgaWYgKHR5cGVvZiBhY3Rpb24gPT09IFwic3RyaW5nXCIpIHtcbiAgICAgIC8vIEV4ZWN1dGUgYXMgc2hlbGwgY29tbWFuZFxuICAgICAgY29uc3QgeyBzdGRvdXQsIHN0ZGVyciB9ID0gYXdhaXQgZXhlY0FzeW5jKGFjdGlvbik7XG4gICAgICBpZiAoc3Rkb3V0KSBjb25zb2xlLmxvZyhzdGRvdXQudHJpbSgpKTtcbiAgICAgIGlmIChzdGRlcnIpIGNvbnNvbGUuZXJyb3Ioc3RkZXJyLnRyaW0oKSk7XG4gICAgfSBlbHNlIHtcbiAgICAgIC8vIEV4ZWN1dGUgYXMgZnVuY3Rpb25cbiAgICAgIGF3YWl0IGFjdGlvbigpO1xuICAgIH1cbiAgfVxuXG4gIGFzeW5jIGZ1bmN0aW9uIHJ1bkFsbFN0ZXBzKCk6IFByb21pc2U8dm9pZD4ge1xuICAgIGZvciAoY29uc3Qgc3RlcCBvZiBzdGVwcykge1xuICAgICAgaWYgKHN0b3BwaW5nKSBicmVhaztcbiAgICAgIGNvbnN0IHN0YXJ0ID0gRGF0ZS5ub3coKTtcbiAgICAgIHRyeSB7XG4gICAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSAke3N0ZXAubmFtZX0g4o+zYCk7XG4gICAgICAgIGF3YWl0IGV4ZWN1dGVBY3Rpb24oc3RlcC5hY3Rpb24pO1xuICAgICAgICBjb25zb2xlLmxvZyhgW2FweF0gJHtzdGVwLm5hbWV9IOKckyAoJHtEYXRlLm5vdygpIC0gc3RhcnR9IG1zKWApO1xuICAgICAgfSBjYXRjaCAoZXJyKSB7XG4gICAgICAgIGNvbnNvbGUuZXJyb3IoYFthcHhdICR7c3RlcC5uYW1lfSDinJdgLCBlcnIpO1xuICAgICAgICB0aHJvdyBlcnI7XG4gICAgICB9XG4gICAgfVxuICB9XG5cbiAgZnVuY3Rpb24gZW5zdXJlR2l0aWdub3JlSW5PdXREaXIoKTogdm9pZCB7XG4gICAgaWYgKCFvdXREaXIpIHJldHVybjtcbiAgICBcbiAgICAvLyBDcmVhdGUgdGhlIG91dHB1dCBkaXJlY3RvcnkgaWYgaXQgZG9lc24ndCBleGlzdFxuICAgIGlmICghZXhpc3RzU3luYyhvdXREaXIpKSB7XG4gICAgICBta2RpclN5bmMob3V0RGlyLCB7IHJlY3Vyc2l2ZTogdHJ1ZSB9KTtcbiAgICB9XG5cbiAgICAvLyBFbnN1cmUgLmdpdGlnbm9yZSBleGlzdHMgaW4gb3V0cHV0IGRpcmVjdG9yeVxuICAgIGNvbnN0IGdpdGlnbm9yZVBhdGggPSBqb2luKG91dERpciwgXCIuZ2l0aWdub3JlXCIpO1xuICAgIGlmICghZXhpc3RzU3luYyhnaXRpZ25vcmVQYXRoKSkge1xuICAgICAgd3JpdGVGaWxlU3luYyhnaXRpZ25vcmVQYXRoLCBcIipcXG5cIik7XG4gICAgICBjb25zb2xlLmxvZyhgW2FweF0gZW5zdXJlZCAke2dpdGlnbm9yZVBhdGh9YCk7XG4gICAgfVxuICB9XG5cbiAgZnVuY3Rpb24gc3RvcCgpOiB2b2lkIHtcbiAgICBpZiAoc3RvcHBpbmcpIHJldHVybjtcbiAgICBzdG9wcGluZyA9IHRydWU7XG4gICAgaWYgKHRpbWVyKSBjbGVhclRpbWVvdXQodGltZXIpO1xuICAgIGNvbnNvbGUubG9nKFwiW2FweF0gc3RvcHBpbmcuLi5cIik7XG4gIH1cblxuICByZXR1cm4ge1xuICAgIG5hbWU6IFwiYXB4XCIsXG4gICAgYXBwbHk6ICgpID0+IHRydWUsXG4gICAgXG4gICAgY29uZmlnUmVzb2x2ZWQoY29uZmlnKSB7XG4gICAgICBvdXREaXIgPSBjb25maWcuYnVpbGQub3V0RGlyO1xuICAgICAgcmVzb2x2ZWRJZ25vcmVzID0gaWdub3JlLm1hcCgocGF0dGVybikgPT4gcmVzb2x2ZShwcm9jZXNzLmN3ZCgpLCBwYXR0ZXJuKSk7XG4gICAgICBcbiAgICAgIC8vIFNldHVwIHNpZ25hbCBoYW5kbGVycyBmb3IgZ3JhY2VmdWwgc2h1dGRvd25cbiAgICAgIHByb2Nlc3Mub24oXCJTSUdJTlRcIiwgc3RvcCk7XG4gICAgICBwcm9jZXNzLm9uKFwiU0lHVEVSTVwiLCBzdG9wKTtcbiAgICB9LFxuXG4gICAgY29uZmlndXJlU2VydmVyKHNlcnZlcikge1xuICAgICAgc2VydmVyLmh0dHBTZXJ2ZXI/Lm9uY2UoXCJjbG9zZVwiLCBzdG9wKTtcbiAgICB9LFxuXG4gICAgYXN5bmMgYnVpbGRTdGFydCgpIHtcbiAgICAgIGVuc3VyZUdpdGlnbm9yZUluT3V0RGlyKCk7XG4gICAgICBcbiAgICAgIGlmIChzdGVwcy5sZW5ndGggPiAwKSB7XG4gICAgICAgIGF3YWl0IHJ1bkFsbFN0ZXBzKCk7XG4gICAgICB9XG4gICAgfSxcblxuICAgIGhhbmRsZUhvdFVwZGF0ZShjdHgpIHtcbiAgICAgIC8vIENoZWNrIGlmIGZpbGUgc2hvdWxkIGJlIGlnbm9yZWRcbiAgICAgIGlmIChyZXNvbHZlZElnbm9yZXMuc29tZSgocGF0dGVybikgPT4gY3R4LmZpbGUuaW5jbHVkZXMocGF0dGVybikpKSB7XG4gICAgICAgIHJldHVybjtcbiAgICAgIH1cblxuICAgICAgLy8gRGVib3VuY2Ugc3RlcCBleGVjdXRpb24gb24gSE1SIHVwZGF0ZXNcbiAgICAgIGlmICh0aW1lcikgY2xlYXJUaW1lb3V0KHRpbWVyKTtcbiAgICAgIHRpbWVyID0gc2V0VGltZW91dCgoKSA9PiB7XG4gICAgICAgIHRpbWVyID0gbnVsbDtcbiAgICAgICAgdm9pZCBydW5BbGxTdGVwcygpO1xuICAgICAgfSwgMTAwKTtcbiAgICB9LFxuXG4gICAgY2xvc2VCdW5kbGUoKSB7XG4gICAgICBlbnN1cmVHaXRpZ25vcmVJbk91dERpcigpO1xuICAgICAgc3RvcCgpO1xuICAgIH0sXG4gIH07XG59XG5cbi8vIERlZmF1bHQgZXhwb3J0IGZvciBjb252ZW5pZW5jZTogaW1wb3J0IGFweCBmcm9tIFwiYXB4XCJcbmV4cG9ydCBkZWZhdWx0IGFweDsiCiAgXSwKICAibWFwcGluZ3MiOiAiO0FBQUE7QUFDQTtBQUVBO0FBQ0E7QUFrQk8sU0FBUyxHQUFHLENBQUMsVUFBNEIsQ0FBQyxHQUFXO0FBQzFELFVBQVEsUUFBUSxDQUFDLEdBQUcsU0FBUyxDQUFDLE1BQU07QUFFcEMsTUFBSTtBQUNKLE1BQUksUUFBK0I7QUFDbkMsTUFBSSxXQUFXO0FBQ2YsTUFBSSxrQkFBNEIsQ0FBQztBQUVqQyxpQkFBZSxhQUFhLENBQUMsUUFBbUM7QUFDOUQsZUFBVyxXQUFXLFVBQVU7QUFFOUIsY0FBUSxRQUFRLFdBQVcsTUFBTSxVQUFVLE1BQU07QUFDakQsVUFBSTtBQUFRLGdCQUFRLElBQUksT0FBTyxLQUFLLENBQUM7QUFDckMsVUFBSTtBQUFRLGdCQUFRLE1BQU0sT0FBTyxLQUFLLENBQUM7QUFBQSxJQUN6QyxPQUFPO0FBRUwsWUFBTSxPQUFPO0FBQUE7QUFBQTtBQUlqQixpQkFBZSxXQUFXLEdBQWtCO0FBQzFDLGVBQVcsUUFBUSxPQUFPO0FBQ3hCLFVBQUk7QUFBVTtBQUNkLFlBQU0sUUFBUSxLQUFLLElBQUk7QUFDdkIsVUFBSTtBQUNGLGdCQUFRLElBQUksU0FBUyxLQUFLLGFBQU87QUFDakMsY0FBTSxjQUFjLEtBQUssTUFBTTtBQUMvQixnQkFBUSxJQUFJLFNBQVMsS0FBSyxnQkFBVSxLQUFLLElBQUksSUFBSSxXQUFXO0FBQUEsZUFDckQsS0FBUDtBQUNBLGdCQUFRLE1BQU0sU0FBUyxLQUFLLGVBQVMsR0FBRztBQUN4QyxjQUFNO0FBQUE7QUFBQSxJQUVWO0FBQUE7QUFHRixXQUFTLHVCQUF1QixHQUFTO0FBQ3ZDLFNBQUs7QUFBUTtBQUdiLFNBQUssV0FBVyxNQUFNLEdBQUc7QUFDdkIsZ0JBQVUsUUFBUSxFQUFFLFdBQVcsS0FBSyxDQUFDO0FBQUEsSUFDdkM7QUFHQSxVQUFNLGdCQUFnQixLQUFLLFFBQVEsWUFBWTtBQUMvQyxTQUFLLFdBQVcsYUFBYSxHQUFHO0FBQzlCLG9CQUFjLGVBQWUsS0FBSztBQUNsQyxjQUFRLElBQUksaUJBQWlCLGVBQWU7QUFBQSxJQUM5QztBQUFBO0FBR0YsV0FBUyxJQUFJLEdBQVM7QUFDcEIsUUFBSTtBQUFVO0FBQ2QsZUFBVztBQUNYLFFBQUk7QUFBTyxtQkFBYSxLQUFLO0FBQzdCLFlBQVEsSUFBSSxtQkFBbUI7QUFBQTtBQUdqQyxTQUFPO0FBQUEsSUFDTCxNQUFNO0FBQUEsSUFDTixPQUFPLE1BQU07QUFBQSxJQUViLGNBQWMsQ0FBQyxRQUFRO0FBQ3JCLGVBQVMsT0FBTyxNQUFNO0FBQ3RCLHdCQUFrQixPQUFPLElBQUksQ0FBQyxZQUFZLFFBQVEsUUFBUSxJQUFJLEdBQUcsT0FBTyxDQUFDO0FBR3pFLGNBQVEsR0FBRyxVQUFVLElBQUk7QUFDekIsY0FBUSxHQUFHLFdBQVcsSUFBSTtBQUFBO0FBQUEsSUFHNUIsZUFBZSxDQUFDLFFBQVE7QUFDdEIsYUFBTyxZQUFZLEtBQUssU0FBUyxJQUFJO0FBQUE7QUFBQSxTQUdqQyxXQUFVLEdBQUc7QUFDakIsOEJBQXdCO0FBRXhCLFVBQUksTUFBTSxTQUFTLEdBQUc7QUFDcEIsY0FBTSxZQUFZO0FBQUEsTUFDcEI7QUFBQTtBQUFBLElBR0YsZUFBZSxDQUFDLEtBQUs7QUFFbkIsVUFBSSxnQkFBZ0IsS0FBSyxDQUFDLFlBQVksSUFBSSxLQUFLLFNBQVMsT0FBTyxDQUFDLEdBQUc7QUFDakU7QUFBQSxNQUNGO0FBR0EsVUFBSTtBQUFPLHFCQUFhLEtBQUs7QUFDN0IsY0FBUSxXQUFXLE1BQU07QUFDdkIsZ0JBQVE7QUFDUixRQUFLLFlBQVk7QUFBQSxTQUNoQixHQUFHO0FBQUE7QUFBQSxJQUdSLFdBQVcsR0FBRztBQUNaLDhCQUF3QjtBQUN4QixXQUFLO0FBQUE7QUFBQSxFQUVUO0FBQUE7QUFySEYsSUFBTSxZQUFZLFVBQVUsSUFBSTtBQVN6QixJQUFNLE9BQU8sQ0FBQyxTQUE2QjtBQWdIbEQsSUFBZTsiLAogICJkZWJ1Z0lkIjogIkVBMzY0NzM1RUZFMTBFQzQ2NDc1NkUyMTY0NzU2RTIxIiwKICAibmFtZXMiOiBbXQp9
