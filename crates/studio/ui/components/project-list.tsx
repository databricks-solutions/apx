import { ProjectCard } from "@/components/project-card";
import type { ProjectInfo } from "@/lib/tauri";

interface ProjectListProps {
  projects: ProjectInfo[];
  loading: boolean;
}

export function ProjectList({ projects, loading }: ProjectListProps) {
  if (loading && projects.length === 0) {
    return (
      <div className="flex items-center justify-center py-20">
        <div className="text-muted-foreground">Loading projects...</div>
      </div>
    );
  }

  return (
    <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
      {projects.map((project) => (
        <ProjectCard key={project.path} project={project} />
      ))}
    </div>
  );
}
