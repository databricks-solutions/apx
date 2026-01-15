import { useEffect } from "react";

type LogLevel = "error" | "warn";
type LogSource = "console" | "window" | "promise";
type LogPayload = {
  level: LogLevel;
  source: LogSource;
  message: string;
  stack?: string;
  timestamp: number;
};

function sendLog(payload: LogPayload) {
  const bodyText = JSON.stringify(payload);
  if (navigator.sendBeacon) {
    const blob = new Blob([bodyText], { type: "application/json" });
    navigator.sendBeacon("/__apx__/browser-logs", blob);
  } else {
    fetch("/__apx__/browser-logs", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: bodyText,
      keepalive: true,
    }).catch(() => {});
  }
}

function formatError(error: unknown): { message: string; stack?: string } {
  if (error instanceof Error) {
    return { message: error.message, stack: error.stack };
  }
  return { message: String(error) };
}

export function ApxDevtools() {
  useEffect(() => {
    if (!import.meta.env.DEV) return;

    const originalError = console.error;

    // Intercept console.error
    console.error = (...args: unknown[]) => {
      originalError.apply(console, args);
      const { message, stack } = formatError(args[0]);
      sendLog({
        level: "error",
        source: "console",
        message: args.map(String).join(" ") || message,
        stack,
        timestamp: Date.now(),
      });
    };

    // Window error handler (uncaught exceptions)
    const onError = (event: ErrorEvent) => {
      sendLog({
        level: "error",
        source: "window",
        message: event.message,
        stack: event.error?.stack,
        timestamp: Date.now(),
      });
    };

    // Unhandled promise rejection handler
    const onRejection = (event: PromiseRejectionEvent) => {
      const { message, stack } = formatError(event.reason);
      sendLog({
        level: "error",
        source: "promise",
        message,
        stack,
        timestamp: Date.now(),
      });
    };

    window.addEventListener("error", onError);
    window.addEventListener("unhandledrejection", onRejection);

    return () => {
      console.error = originalError;
      window.removeEventListener("error", onError);
      window.removeEventListener("unhandledrejection", onRejection);
    };
  }, []);

  return null;
}
