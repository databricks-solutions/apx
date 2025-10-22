import { createFileRoute, Outlet } from "@tanstack/react-router";

export const Route = createFileRoute("/_sidebar")({
    component: () => <Layout />,
});

function Layout() {
    return (
        <div>
            <Outlet />
        </div>
    );
}