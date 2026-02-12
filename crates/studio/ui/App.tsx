import { useState, useEffect, useCallback } from "react";
import { Header } from "@/components/header";
import { ProjectList } from "@/components/project-list";
import { EmptyState } from "@/components/empty-state";
import { getProjects, refreshProjects, type ProjectInfo } from "@/lib/tauri";

export default function App() {
  const [projects, setProjects] = useState<ProjectInfo[]>([]);
  const [loading, setLoading] = useState(true);

  const load = useCallback(async () => {
    try {
      const data = await getProjects();
      setProjects(data);
    } catch (err) {
      console.error("Failed to load projects:", err);
    } finally {
      setLoading(false);
    }
  }, []);

  const handleRefresh = useCallback(async () => {
    setLoading(true);
    try {
      const data = await refreshProjects();
      setProjects(data);
    } catch (err) {
      console.error("Failed to refresh projects:", err);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  return (
    <div className="min-h-screen flex flex-col">
      <Header onRefresh={handleRefresh} loading={loading} />
      <main className="flex-1 px-6 py-8 max-w-6xl mx-auto w-full">
        {!loading && projects.length === 0 ? (
          <EmptyState />
        ) : (
          <ProjectList projects={projects} loading={loading} />
        )}
      </main>
    </div>
  );
}
