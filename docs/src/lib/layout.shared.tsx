import type { BaseLayoutProps } from "fumadocs-ui/layouts/shared";

export function baseOptions(): BaseLayoutProps {
  return {
    nav: {
      title: (
        <div className="flex items-center gap-2">
          <img src="/logo.svg" alt="apx" className="w-6 h-6 rounded" />
        </div>
      ),
    },
    links: [
      {
        text: "Documentation",
        url: "/docs",
      },
      {
        text: "GitHub",
        url: "https://github.com/databricks-solutions/apx",
        external: true,
      },
    ],
    githubUrl: "https://github.com/databricks-solutions/apx",
  };
}
