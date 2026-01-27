import { createFileRoute } from "@tanstack/react-router";
import Logo from "@/components/apx/logo";

export const Route = createFileRoute("/")({
  component: () => <Index />,
});

function Index() {
  return (
    <div className="min-h-screen bg-background flex flex-col items-center justify-center p-8">
      {/* Logo and App Name */}
      <div className="mb-8">
        <Logo to={undefined} className="text-2xl" />
      </div>

      {/* Welcome Message */}
      <h1 className="text-4xl md:text-5xl font-bold text-foreground text-center mb-4">
        Welcome to {__APP_NAME__}
      </h1>

      <p className="text-muted-foreground text-center max-w-md mb-12">
        Get started by editing{" "}
        <code className="bg-muted px-2 py-1 rounded text-sm">
          src/{__APP_SLUG__}/ui/routes/index.tsx
        </code>
      </p>

      {/* Built with apx card */}
      <a
        href="https://github.com/databricks-solutions/apx"
        target="_blank"
        rel="noopener noreferrer"
        className="fixed bottom-8 right-8"
      >
        <div className="flex items-center gap-3 px-4 py-3 rounded-lg border border-border bg-card hover:bg-accent transition-colors shadow-sm">
          <img
            src="https://raw.githubusercontent.com/databricks-solutions/apx/refs/heads/main/assets/logo.svg"
            className="h-8 w-8"
            alt="apx logo"
          />
          <div className="flex flex-col items-start">
            <span className="text-xs text-muted-foreground font-medium">
              Built with
            </span>
            <span className="text-sm font-semibold text-foreground">apx</span>
          </div>
        </div>
      </a>
    </div>
  );
}
