import { invoke } from "@tauri-apps/api/core";

export interface ProjectInfo {
  path: string;
  name: string;
  port: number;
  exists: boolean;
}

export async function getProjects(): Promise<ProjectInfo[]> {
  return invoke<ProjectInfo[]>("get_projects");
}

export async function refreshProjects(): Promise<ProjectInfo[]> {
  return invoke<ProjectInfo[]>("refresh_projects");
}
