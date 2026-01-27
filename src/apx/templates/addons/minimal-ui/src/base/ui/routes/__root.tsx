import { QueryClient } from "@tanstack/react-query";
import { createRootRouteWithContext, Outlet } from "@tanstack/react-router";
import { TanStackRouterDevtools } from "@tanstack/react-router-devtools";
import { Toaster } from "sonner";

export const Route = createRootRouteWithContext<{
  queryClient: QueryClient;
}>()({
  component: () => (
    <>
      {import.meta.env.DEV && (
        <>
          <TanStackRouterDevtools position="bottom-right" />
        </>
      )}
      <Outlet />
      <Toaster richColors />
    </>
  ),
});
