import { Folder, FolderX } from "lucide-react";
import {
  Card,
  CardHeader,
  CardTitle,
  CardContent,
  CardFooter,
} from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import {
  Tooltip,
  TooltipTrigger,
  TooltipContent,
} from "@/components/ui/tooltip";
import type { ProjectInfo } from "@/lib/tauri";

interface ProjectCardProps {
  project: ProjectInfo;
}

export function ProjectCard({ project }: ProjectCardProps) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Card className="group relative overflow-hidden transition-all hover:scale-[1.02] hover:border-purple-500/50 hover:shadow-xl hover:shadow-purple-500/10 cursor-default">
          <div className="absolute inset-0 bg-gradient-to-br from-blue-500/0 to-purple-500/0 group-hover:from-blue-500/5 group-hover:to-purple-500/5 transition-all" />
          <CardHeader className="relative pb-3">
            <CardTitle className="flex items-center gap-2 text-base">
              {project.exists ? (
                <Folder className="h-4 w-4 text-blue-400" />
              ) : (
                <FolderX className="h-4 w-4 text-destructive" />
              )}
              {project.name}
            </CardTitle>
          </CardHeader>
          <CardContent className="relative pb-3">
            <p className="text-sm text-muted-foreground font-mono truncate">
              {project.path}
            </p>
          </CardContent>
          <CardFooter className="relative">
            <Badge variant="secondary" className="font-mono text-xs">
              :{project.port}
            </Badge>
            {!project.exists && (
              <Badge variant="destructive" className="ml-2 text-xs">
                missing
              </Badge>
            )}
          </CardFooter>
        </Card>
      </TooltipTrigger>
      <TooltipContent>
        <p className="font-mono text-xs">{project.path}</p>
      </TooltipContent>
    </Tooltip>
  );
}
