import Link from "next/link";
import {
  Terminal,
  Server,
  Sparkles,
  ArrowRight,
  Github,
  Rocket,
  Bot,
  Code2,
  Package,
  Layers,
  Database,
  Palette,
  Play,
  ChevronRight,
} from "lucide-react";
import { Python } from "@/components/icons/python";
import { React as ReactIcon } from "@/components/icons/react";
import { TypeScript } from "@/components/icons/typescript";
import { FastAPI } from "@/components/icons/fast-api";
import { shadcnui } from "@/components/icons/shadcn-ui";
import { Bun } from "@/components/icons/bun";
import { UV } from "@/components/icons/uv";

const techStack = [
  { name: "Python", icon: Python },
  { name: "FastAPI", icon: FastAPI },
  { name: "React", icon: ReactIcon },
  { name: "TypeScript", icon: TypeScript },
  { name: "shadcn/ui", icon: shadcnui },
  { name: "Bun", icon: Bun },
  { name: "uv", icon: UV },
];

export default function HomePage() {
  return (
    <main className="flex flex-1 flex-col">
      {/* Hero Section */}
      <section className="flex flex-col items-center justify-center px-6 py-16 md:py-24 text-center">
        {/* Logo */}
        <div className="mb-8">
          <img
            src="/logo.svg"
            alt="apx logo"
            className="w-24 h-24 md:w-32 md:h-32 rounded-2xl shadow-lg"
          />
        </div>

        {/* Tagline */}
        <p className="text-lg md:text-xl text-fd-muted-foreground max-w-2xl mb-6">
          Reliable, feature-full, human and LLM friendly development toolkit for
          building{" "}
          <span className="text-fd-foreground font-medium">
            Databricks Apps
          </span>{" "}
          🚀
        </p>

        {/* Description */}
        <p className="text-fd-muted-foreground max-w-xl mb-8">
          <Bot className="inline w-4 h-4 mr-1" />
          Build, develop, and deploy modern full-stack applications with Python
          backend and React frontend. Designed for both human developers and AI
          assistants.
        </p>

        {/* CTA Buttons */}
        <div className="flex flex-wrap gap-4 justify-center">
          <Link
            href="/docs"
            className="inline-flex items-center justify-center gap-2 rounded-lg bg-fd-primary px-6 py-3 text-sm font-medium text-fd-primary-foreground shadow-sm transition-colors hover:bg-fd-primary/90"
          >
            <Rocket className="w-4 h-4" />
            Get Started
            <ArrowRight className="w-4 h-4" />
          </Link>
          <Link
            href="https://github.com/databricks-solutions/apx"
            className="inline-flex items-center justify-center gap-2 rounded-lg border border-fd-border bg-fd-background px-6 py-3 text-sm font-medium text-fd-foreground shadow-sm transition-colors hover:bg-fd-accent"
          >
            <Github className="w-4 h-4" />
            View on GitHub
          </Link>
        </div>
      </section>

      {/* Quick Start */}
      <section className="px-6 py-8 bg-fd-muted/30">
        <div className="max-w-2xl mx-auto">
          <div className="rounded-lg border border-fd-border bg-fd-card p-4 font-mono text-sm">
            <div className="flex items-center gap-2 text-fd-muted-foreground mb-2">
              <Terminal className="w-4 h-4" />
              <span>Quick Start</span>
              <Play className="w-3 h-3 ml-auto" />
            </div>
            <code className="text-fd-foreground break-all">
              uvx --index https://databricks-solutions.github.io/apx/simple apx
              init
            </code>
          </div>
        </div>
      </section>

      {/* Features Grid */}
      <section className="px-6 py-16">
        <div className="max-w-5xl mx-auto">
          <h2 className="text-2xl md:text-3xl font-bold text-center mb-4 flex items-center justify-center gap-2">
            <Package className="w-7 h-7 text-fd-primary" />
            Everything you need
          </h2>
          <p className="text-fd-muted-foreground text-center max-w-2xl mx-auto mb-12">
            apx provides a complete toolkit for building production-ready
            Databricks Apps
          </p>

          <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-6">
            {/* CLI Reference */}
            <Link
              href="/docs/cli"
              className="group flex flex-col rounded-xl border border-fd-border bg-fd-card p-6 transition-colors hover:bg-fd-accent hover:border-fd-accent-foreground/20"
            >
              <div className="flex items-center gap-3 mb-3">
                <div className="flex items-center justify-center w-10 h-10 rounded-lg bg-fd-primary/10 text-fd-primary">
                  <Terminal className="w-5 h-5" />
                </div>
                <h3 className="font-semibold text-fd-foreground">
                  CLI Reference
                </h3>
                <ChevronRight className="w-4 h-4 ml-auto text-fd-muted-foreground group-hover:text-fd-foreground transition-colors" />
              </div>
              <p className="text-sm text-fd-muted-foreground">
                <Code2 className="inline w-3 h-3 mr-1" />
                Initialize projects, build for production, and manage your
                development workflow with powerful CLI commands.
              </p>
            </Link>

            {/* MCP Reference */}
            <Link
              href="/docs/mcp"
              className="group flex flex-col rounded-xl border border-fd-border bg-fd-card p-6 transition-colors hover:bg-fd-accent hover:border-fd-accent-foreground/20"
            >
              <div className="flex items-center gap-3 mb-3">
                <div className="flex items-center justify-center w-10 h-10 rounded-lg bg-fd-primary/10 text-fd-primary">
                  <Server className="w-5 h-5" />
                </div>
                <h3 className="font-semibold text-fd-foreground">
                  MCP Reference
                </h3>
                <ChevronRight className="w-4 h-4 ml-auto text-fd-muted-foreground group-hover:text-fd-foreground transition-colors" />
              </div>
              <p className="text-sm text-fd-muted-foreground">
                <Bot className="inline w-3 h-3 mr-1" />
                Connect AI assistants to your development workflow with the
                Model Context Protocol server.
              </p>
            </Link>

            {/* APX Features */}
            <Link
              href="/docs/features"
              className="group flex flex-col rounded-xl border border-fd-border bg-fd-card p-6 transition-colors hover:bg-fd-accent hover:border-fd-accent-foreground/20"
            >
              <div className="flex items-center gap-3 mb-3">
                <div className="flex items-center justify-center w-10 h-10 rounded-lg bg-fd-primary/10 text-fd-primary">
                  <Sparkles className="w-5 h-5" />
                </div>
                <h3 className="font-semibold text-fd-foreground">Features</h3>
                <ChevronRight className="w-4 h-4 ml-auto text-fd-muted-foreground group-hover:text-fd-foreground transition-colors" />
              </div>
              <p className="text-sm text-fd-muted-foreground">
                <Database className="inline w-3 h-3 mr-1" />
                Dev server, built-in components CLI, local dev database, and
                more productivity features.
              </p>
            </Link>
          </div>
        </div>
      </section>

      {/* Tech Stack */}
      <section className="px-6 py-16 bg-fd-muted/30">
        <div className="max-w-5xl mx-auto text-center">
          <h2 className="text-2xl md:text-3xl font-bold mb-4 flex items-center justify-center gap-2">
            <Layers className="w-7 h-7 text-fd-primary" />
            Built on modern stack
          </h2>
          <p className="text-fd-muted-foreground max-w-2xl mx-auto mb-8">
            <Palette className="inline w-4 h-4 mr-1" />
            Leverage the best tools in the Python and JavaScript ecosystems
          </p>

          <div className="flex flex-wrap justify-center gap-4">
            {techStack.map(({ name, icon: Icon }) => (
              <span
                key={name}
                className="inline-flex items-center gap-2 rounded-full border border-fd-border bg-fd-background px-4 py-2 text-sm font-medium text-fd-foreground"
              >
                <Icon className="w-4 h-4" />
                {name}
              </span>
            ))}
          </div>
        </div>
      </section>

      {/* Footer */}
      <footer className="px-6 py-8 border-t border-fd-border">
        <div className="max-w-5xl mx-auto text-center text-sm text-fd-muted-foreground">
          <p>
            © 2026 Databricks Solutions.{" "}
            <Link
              href="https://github.com/databricks-solutions/apx/blob/main/LICENSE.md"
              className="underline hover:text-fd-foreground transition-colors"
            >
              License
            </Link>
          </p>
        </div>
      </footer>
    </main>
  );
}
