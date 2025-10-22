import SidebarLayout from "@/components/apx/SidebarLayout";
import { createFileRoute, Link, useLocation } from "@tanstack/react-router";
import { cn } from "@/lib/utils";
import { User } from "lucide-react";
import {
    SidebarGroup,
    SidebarGroupContent,
    SidebarMenu,
    SidebarMenuItem,
} from "@/components/ui/sidebar";

export const Route = createFileRoute("/_sidebar")({
    component: () => <Layout />,
});

function Layout() {
    const location = useLocation();

    const navItems = [
        {
            to: "/profile",
            label: "Profile",
            icon: <User size={16} />,
            match: (path: string) => path === "/profile",
        },
    ];

    return (
        <SidebarLayout>
            <SidebarGroup>
                <SidebarGroupContent>
                    <SidebarMenu>
                        {navItems.map((item) => (
                            <SidebarMenuItem key={item.to}>
                                <Link
                                    to={item.to}
                                    className={cn(
                                        "flex items-center gap-2 p-2 rounded-lg",
                                        item.match(location.pathname)
                                            ? "bg-sidebar-accent text-sidebar-accent-foreground"
                                            : "text-sidebar-foreground hover:bg-sidebar-accent hover:text-sidebar-accent-foreground",
                                    )}
                                >
                                    {item.icon}
                                    <span>{item.label}</span>
                                </Link>
                            </SidebarMenuItem>
                        ))}
                    </SidebarMenu>
                </SidebarGroupContent>
            </SidebarGroup>
        </SidebarLayout>
    );
}