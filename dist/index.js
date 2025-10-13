// src/apx/plugins/ensure-gitignore.ts
import { existsSync, mkdirSync, writeFileSync } from "fs";
import { join } from "path";
function ensureGitignore() {
  let outDir;
  return {
    name: "ensure-gitignore",
    apply: "build",
    configResolved(config) {
      outDir = config.build.outDir;
    },
    buildStart: () => {
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
    }
  };
}
// src/apx/plugins/run-on-reload.ts
import { resolve } from "path";
function runOnReload({
  steps,
  ignore = []
}) {
  let timer = null;
  let stopping = false;
  let resolvedIgnores = ignore.map((i) => resolve(__dirname, i));
  async function runAll() {
    for (const s of steps) {
      if (stopping)
        break;
      const start = Date.now();
      try {
        console.log(`[vite-plugin-run] ${s.name} \u23F3`);
        await s.action();
        console.log(`[vite-plugin-run] ${s.name} \u2713 (${Date.now() - start} ms)`);
      } catch (err) {
        console.error(`[vite-plugin-run] ${s.name} \u2717`, err);
        throw err;
      }
    }
  }
  function stop() {
    if (stopping)
      return;
    stopping = true;
    if (timer)
      clearTimeout(timer);
    console.log("[vite-plugin-run] stopping...");
  }
  return {
    name: "vite-plugin-run",
    apply: () => true,
    configResolved() {
      process.on("SIGINT", stop);
      process.on("SIGTERM", stop);
    },
    configureServer(server) {
      server.httpServer?.once("close", stop);
    },
    async buildStart() {
      await runAll();
    },
    handleHotUpdate(ctx) {
      if (resolvedIgnores.some((i) => ctx.file.includes(i))) {
        return;
      }
      if (timer)
        clearTimeout(timer);
      timer = setTimeout(() => (timer = null, void runAll()), 100);
    },
    closeBundle() {
      stop();
    }
  };
}
var __dirname = "/Users/ivan.trusov/projects/apx/src/apx/plugins";
export {
  runOnReload,
  ensureGitignore
};

//# debugId=BA900144F70B433464756E2164756E21
//# sourceMappingURL=data:application/json;base64,ewogICJ2ZXJzaW9uIjogMywKICAic291cmNlcyI6IFsiLi4vc3JjL2FweC9wbHVnaW5zL2Vuc3VyZS1naXRpZ25vcmUudHMiLCAiLi4vc3JjL2FweC9wbHVnaW5zL3J1bi1vbi1yZWxvYWQudHMiXSwKICAic291cmNlc0NvbnRlbnQiOiBbCiAgICAiaW1wb3J0IHsgZXhpc3RzU3luYywgbWtkaXJTeW5jLCB3cml0ZUZpbGVTeW5jIH0gZnJvbSBcImZzXCI7XG5pbXBvcnQgeyBqb2luIH0gZnJvbSBcInBhdGhcIjtcbmltcG9ydCB7IHR5cGUgUGx1Z2luIH0gZnJvbSBcInZpdGVcIjtcblxuZXhwb3J0IGZ1bmN0aW9uIGVuc3VyZUdpdGlnbm9yZSgpOiBQbHVnaW4ge1xuICBsZXQgb3V0RGlyOiBzdHJpbmc7XG4gIHJldHVybiB7XG4gICAgbmFtZTogXCJlbnN1cmUtZ2l0aWdub3JlXCIsXG4gICAgYXBwbHk6IFwiYnVpbGRcIixcbiAgICBjb25maWdSZXNvbHZlZChjb25maWcpIHtcbiAgICAgIG91dERpciA9IGNvbmZpZy5idWlsZC5vdXREaXI7XG4gICAgfSxcbiAgICBidWlsZFN0YXJ0OiAoKSA9PiB7XG4gICAgICAvLyBjcmVhdGUgdGhlIG91dCBkaXIgaWYgaXQgZG9lc24ndCBleGlzdFxuICAgICAgaWYgKCFleGlzdHNTeW5jKG91dERpcikpIHtcbiAgICAgICAgbWtkaXJTeW5jKG91dERpciwgeyByZWN1cnNpdmU6IHRydWUgfSk7XG4gICAgICB9XG4gICAgICBjb25zdCBpZ25vcmVkID0gam9pbihvdXREaXIsIFwiLmdpdGlnbm9yZVwiKTtcbiAgICAgIHdyaXRlRmlsZVN5bmMoaWdub3JlZCwgXCIqXFxuXCIpO1xuICAgICAgY29uc29sZS5sb2coYFtlbnN1cmUtb3V0LWRpcl0gZW5zdXJlZCAke2lnbm9yZWR9YCk7XG4gICAgfSxcbiAgICBjbG9zZUJ1bmRsZTogKCkgPT4ge1xuICAgICAgY29uc3QgZ2l0aWdub3JlID0gam9pbihvdXREaXIsIFwiLmdpdGlnbm9yZVwiKTtcbiAgICAgIGlmICghZXhpc3RzU3luYyhnaXRpZ25vcmUpKSB7XG4gICAgICAgIHdyaXRlRmlsZVN5bmMoZ2l0aWdub3JlLCBcIipcXG5cIik7XG4gICAgICB9XG4gICAgfSxcbiAgfTtcbn1cbiIsCiAgICAiaW1wb3J0IHsgdHlwZSBQbHVnaW4gfSBmcm9tIFwidml0ZVwiO1xuaW1wb3J0IHsgcmVzb2x2ZSB9IGZyb20gXCJwYXRoXCI7XG5cbmV4cG9ydCB0eXBlIFN0ZXBBY3Rpb24gPSAoKSA9PiB2b2lkIHwgUHJvbWlzZTx2b2lkPjtcbmV4cG9ydCB0eXBlIFN0ZXBTcGVjID0geyBuYW1lOiBzdHJpbmc7IGFjdGlvbjogU3RlcEFjdGlvbiB9O1xuZXhwb3J0IGNvbnN0IFN0ZXAgPSAoczogU3RlcFNwZWMpID0+IHM7XG5cbmV4cG9ydCBmdW5jdGlvbiBydW5PblJlbG9hZCh7XG4gIHN0ZXBzLFxuICBpZ25vcmUgPSBbXSxcbn06IHtcbiAgc3RlcHM6IFN0ZXBTcGVjW107XG4gIGlnbm9yZT86IHN0cmluZ1tdO1xufSk6IFBsdWdpbiB7XG4gIGxldCB0aW1lcjogTm9kZUpTLlRpbWVvdXQgfCBudWxsID0gbnVsbDtcbiAgbGV0IHN0b3BwaW5nID0gZmFsc2U7XG4gIGxldCByZXNvbHZlZElnbm9yZXMgPSBpZ25vcmUubWFwKChpKSA9PiByZXNvbHZlKF9fZGlybmFtZSwgaSkpO1xuXG4gIGFzeW5jIGZ1bmN0aW9uIHJ1bkFsbCgpIHtcbiAgICBmb3IgKGNvbnN0IHMgb2Ygc3RlcHMpIHtcbiAgICAgIGlmIChzdG9wcGluZykgYnJlYWs7XG4gICAgICBjb25zdCBzdGFydCA9IERhdGUubm93KCk7XG4gICAgICB0cnkge1xuICAgICAgICBjb25zb2xlLmxvZyhgW3ZpdGUtcGx1Z2luLXJ1bl0gJHtzLm5hbWV9IOKPs2ApO1xuICAgICAgICBhd2FpdCBzLmFjdGlvbigpO1xuICAgICAgICBjb25zb2xlLmxvZyhgW3ZpdGUtcGx1Z2luLXJ1bl0gJHtzLm5hbWV9IOKckyAoJHtEYXRlLm5vdygpIC0gc3RhcnR9IG1zKWApO1xuICAgICAgfSBjYXRjaCAoZXJyKSB7XG4gICAgICAgIGNvbnNvbGUuZXJyb3IoYFt2aXRlLXBsdWdpbi1ydW5dICR7cy5uYW1lfSDinJdgLCBlcnIpO1xuICAgICAgICB0aHJvdyBlcnI7XG4gICAgICB9XG4gICAgfVxuICB9XG5cbiAgZnVuY3Rpb24gc3RvcCgpIHtcbiAgICBpZiAoc3RvcHBpbmcpIHJldHVybjtcbiAgICBzdG9wcGluZyA9IHRydWU7XG4gICAgaWYgKHRpbWVyKSBjbGVhclRpbWVvdXQodGltZXIpO1xuICAgIGNvbnNvbGUubG9nKFwiW3ZpdGUtcGx1Z2luLXJ1bl0gc3RvcHBpbmcuLi5cIik7XG4gIH1cblxuICByZXR1cm4ge1xuICAgIG5hbWU6IFwidml0ZS1wbHVnaW4tcnVuXCIsXG4gICAgYXBwbHk6ICgpID0+IHRydWUsXG4gICAgY29uZmlnUmVzb2x2ZWQoKSB7XG4gICAgICBwcm9jZXNzLm9uKFwiU0lHSU5UXCIsIHN0b3ApO1xuICAgICAgcHJvY2Vzcy5vbihcIlNJR1RFUk1cIiwgc3RvcCk7XG4gICAgfSxcbiAgICBjb25maWd1cmVTZXJ2ZXIoc2VydmVyKSB7XG4gICAgICBzZXJ2ZXIuaHR0cFNlcnZlcj8ub25jZShcImNsb3NlXCIsIHN0b3ApO1xuICAgIH0sXG4gICAgYXN5bmMgYnVpbGRTdGFydCgpIHtcbiAgICAgIGF3YWl0IHJ1bkFsbCgpO1xuICAgIH0sXG4gICAgaGFuZGxlSG90VXBkYXRlKGN0eCkge1xuICAgICAgaWYgKHJlc29sdmVkSWdub3Jlcy5zb21lKChpKSA9PiBjdHguZmlsZS5pbmNsdWRlcyhpKSkpIHtcbiAgICAgICAgcmV0dXJuO1xuICAgICAgfVxuICAgICAgaWYgKHRpbWVyKSBjbGVhclRpbWVvdXQodGltZXIpO1xuICAgICAgdGltZXIgPSBzZXRUaW1lb3V0KCgpID0+ICgodGltZXIgPSBudWxsKSwgdm9pZCBydW5BbGwoKSksIDEwMCk7XG4gICAgfSxcbiAgICBjbG9zZUJ1bmRsZSgpIHtcbiAgICAgIHN0b3AoKTtcbiAgICB9LFxuICB9O1xufVxuIgogIF0sCiAgIm1hcHBpbmdzIjogIjtBQUFBO0FBQ0E7QUFHTyxTQUFTLGVBQWUsR0FBVztBQUN4QyxNQUFJO0FBQ0osU0FBTztBQUFBLElBQ0wsTUFBTTtBQUFBLElBQ04sT0FBTztBQUFBLElBQ1AsY0FBYyxDQUFDLFFBQVE7QUFDckIsZUFBUyxPQUFPLE1BQU07QUFBQTtBQUFBLElBRXhCLFlBQVksTUFBTTtBQUVoQixXQUFLLFdBQVcsTUFBTSxHQUFHO0FBQ3ZCLGtCQUFVLFFBQVEsRUFBRSxXQUFXLEtBQUssQ0FBQztBQUFBLE1BQ3ZDO0FBQ0EsWUFBTSxVQUFVLEtBQUssUUFBUSxZQUFZO0FBQ3pDLG9CQUFjLFNBQVMsS0FBSztBQUM1QixjQUFRLElBQUksNEJBQTRCLFNBQVM7QUFBQTtBQUFBLElBRW5ELGFBQWEsTUFBTTtBQUNqQixZQUFNLFlBQVksS0FBSyxRQUFRLFlBQVk7QUFDM0MsV0FBSyxXQUFXLFNBQVMsR0FBRztBQUMxQixzQkFBYyxXQUFXLEtBQUs7QUFBQSxNQUNoQztBQUFBO0FBQUEsRUFFSjtBQUFBOztBQzFCRjtBQU1PLFNBQVMsV0FBVztBQUFBLEVBQ3pCO0FBQUEsRUFDQSxTQUFTLENBQUM7QUFBQSxHQUlEO0FBQ1QsTUFBSSxRQUErQjtBQUNuQyxNQUFJLFdBQVc7QUFDZixNQUFJLGtCQUFrQixPQUFPLElBQUksQ0FBQyxNQUFNLFFBQVEsV0FBVyxDQUFDLENBQUM7QUFFN0QsaUJBQWUsTUFBTSxHQUFHO0FBQ3RCLGVBQVcsS0FBSyxPQUFPO0FBQ3JCLFVBQUk7QUFBVTtBQUNkLFlBQU0sUUFBUSxLQUFLLElBQUk7QUFDdkIsVUFBSTtBQUNGLGdCQUFRLElBQUkscUJBQXFCLEVBQUUsYUFBTztBQUMxQyxjQUFNLEVBQUUsT0FBTztBQUNmLGdCQUFRLElBQUkscUJBQXFCLEVBQUUsZ0JBQVUsS0FBSyxJQUFJLElBQUksV0FBVztBQUFBLGVBQzlELEtBQVA7QUFDQSxnQkFBUSxNQUFNLHFCQUFxQixFQUFFLGVBQVMsR0FBRztBQUNqRCxjQUFNO0FBQUE7QUFBQSxJQUVWO0FBQUE7QUFHRixXQUFTLElBQUksR0FBRztBQUNkLFFBQUk7QUFBVTtBQUNkLGVBQVc7QUFDWCxRQUFJO0FBQU8sbUJBQWEsS0FBSztBQUM3QixZQUFRLElBQUksK0JBQStCO0FBQUE7QUFHN0MsU0FBTztBQUFBLElBQ0wsTUFBTTtBQUFBLElBQ04sT0FBTyxNQUFNO0FBQUEsSUFDYixjQUFjLEdBQUc7QUFDZixjQUFRLEdBQUcsVUFBVSxJQUFJO0FBQ3pCLGNBQVEsR0FBRyxXQUFXLElBQUk7QUFBQTtBQUFBLElBRTVCLGVBQWUsQ0FBQyxRQUFRO0FBQ3RCLGFBQU8sWUFBWSxLQUFLLFNBQVMsSUFBSTtBQUFBO0FBQUEsU0FFakMsV0FBVSxHQUFHO0FBQ2pCLFlBQU0sT0FBTztBQUFBO0FBQUEsSUFFZixlQUFlLENBQUMsS0FBSztBQUNuQixVQUFJLGdCQUFnQixLQUFLLENBQUMsTUFBTSxJQUFJLEtBQUssU0FBUyxDQUFDLENBQUMsR0FBRztBQUNyRDtBQUFBLE1BQ0Y7QUFDQSxVQUFJO0FBQU8scUJBQWEsS0FBSztBQUM3QixjQUFRLFdBQVcsT0FBUSxRQUFRLFdBQVksT0FBTyxJQUFJLEdBQUc7QUFBQTtBQUFBLElBRS9ELFdBQVcsR0FBRztBQUNaLFdBQUs7QUFBQTtBQUFBLEVBRVQ7QUFBQTtBQUFBOyIsCiAgImRlYnVnSWQiOiAiQkE5MDAxNDRGNzBCNDMzNDY0NzU2RTIxNjQ3NTZFMjEiLAogICJuYW1lcyI6IFtdCn0=
