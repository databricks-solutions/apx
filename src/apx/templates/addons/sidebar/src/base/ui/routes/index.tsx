import { createFileRoute, Link } from "@tanstack/react-router";
import { Button } from "@/components/ui/button";
import Navbar from "@/components/apx/navbar";
import { User } from "lucide-react";
import { BubbleBackground } from "@/components/backgrounds/bubble";

export const Route = createFileRoute("/")({
  component: () => <Index />,
});

function Index() {
  return (
    <div className="relative h-screen w-screen overflow-hidden flex flex-col">
      {/* Navbar */}
      <Navbar />

      {/* Main content - 2 columns */}
      <main className="flex-1 grid md:grid-cols-2">
        {/* Left column - Gradient only */}
        <BubbleBackground interactive />

        {/* Right column - Content */}
        <div className="relative flex flex-col items-center justify-center p-8 md:p-12 border-l">
          <div className="max-w-lg space-y-8 text-center">
            <h1 className="text-5xl md:text-6xl lg:text-7xl font-bold">
              Welcome to {__APP_NAME__}
            </h1>

            <Button size="lg" asChild>
              <Link to="/profile" className="flex items-center gap-2">
                <User className="h-5 w-5" />
                View <code>/profile</code>
              </Link>
            </Button>
          </div>

          {/* APX Card Button - Bottom Right */}
          <a
            href="https://github.com/databricks-solutions/apx"
            target="_blank"
            rel="noopener noreferrer"
            className="absolute bottom-12 right-2 w-38 group"
          >
            <div className="flex items-center gap-3 px-4 py-3 rounded-lg border bg-card hover:bg-accent transition-colors">
              <img
                src="https://raw.githubusercontent.com/databricks-solutions/apx/refs/heads/main/assets/logo.svg"
                className="h-8 w-8"
                alt="apx logo"
              />
              <div className="flex flex-col items-start text-balance">
                <span className="text-xs font-medium">Built with</span>
                <span className="text-sm font-semibold">apx</span>
              </div>
            </div>
          </a>
        </div>
      </main>

      {/* Background */}
      <div className="absolute inset-0 -z-10 h-full w-full bg-background" />
    </div>
  );
}
