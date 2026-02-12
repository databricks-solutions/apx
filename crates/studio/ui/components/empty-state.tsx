import { FolderOpen } from "lucide-react";

export function EmptyState() {
  return (
    <div className="flex flex-col items-center justify-center py-20 text-center">
      <div className="flex items-center justify-center w-16 h-16 rounded-2xl bg-muted mb-6">
        <FolderOpen className="h-8 w-8 text-muted-foreground" />
      </div>
      <h2 className="text-xl font-semibold mb-2">No projects yet</h2>
      <p className="text-muted-foreground max-w-sm">
        Start a development server to see your projects here.
      </p>
      <code className="mt-4 px-4 py-2 rounded-lg bg-muted font-mono text-sm text-muted-foreground">
        apx dev start
      </code>
    </div>
  );
}
