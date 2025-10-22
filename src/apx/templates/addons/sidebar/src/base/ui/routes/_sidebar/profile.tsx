import { createFileRoute, Link } from "@tanstack/react-router";
import { Suspense } from "react";
import { QueryErrorResetBoundary } from "@tanstack/react-query";
import { ErrorBoundary } from "react-error-boundary";
import { useCurrentUserSuspense } from "@/lib/api";
import selector from "@/lib/selector";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
import { Skeleton } from "@/components/ui/skeleton";
import { Separator } from "@/components/ui/separator";
import { ModeToggle } from "@/components/apx/mode-toggle";
import {
  User,
  Mail,
  Shield,
  Users,
  AlertCircle,
  ArrowLeft,
  CheckCircle,
  XCircle,
} from "lucide-react";

export const Route = createFileRoute("/profile")({
  component: () => <Profile />,
});

function ProfileContent() {
  const { data: user } = useCurrentUserSuspense(selector());

  const getInitials = () => {
    if (user.name?.given_name && user.name?.family_name) {
      return `${user.name.given_name[0]}${user.name.family_name[0]}`.toUpperCase();
    }
    if (user.display_name) {
      const parts = user.display_name.split(" ");
      if (parts.length >= 2) {
        return `${parts[0][0]}${parts[parts.length - 1][0]}`.toUpperCase();
      }
      return user.display_name.substring(0, 2).toUpperCase();
    }
    if (user.user_name) {
      return user.user_name.substring(0, 2).toUpperCase();
    }
    return "U";
  };

  const primaryEmail = user.emails?.find((e) => e.primary)?.value ||
                       user.emails?.[0]?.value ||
                       "No email available";

  return (
    <div className="max-w-4xl mx-auto space-y-6">
      {/* Header Card */}
      <Card className="border-primary/20">
        <CardContent className="pt-6">
          <div className="flex flex-col md:flex-row items-center gap-6">
            <Avatar className="h-24 w-24">
              <AvatarFallback className="text-2xl font-bold bg-primary/10">
                {getInitials()}
              </AvatarFallback>
            </Avatar>
            <div className="flex-1 text-center md:text-left space-y-2">
              <div className="flex items-center gap-3 justify-center md:justify-start">
                <h1 className="text-3xl font-bold">
                  {user.display_name || user.user_name || "User"}
                </h1>
                {user.active !== null && (
                  <Badge
                    variant={user.active ? "default" : "secondary"}
                    className="flex items-center gap-1"
                  >
                    {user.active ? (
                      <>
                        <CheckCircle className="h-3 w-3" />
                        Active
                      </>
                    ) : (
                      <>
                        <XCircle className="h-3 w-3" />
                        Inactive
                      </>
                    )}
                  </Badge>
                )}
              </div>
              {user.user_name && (
                <p className="text-muted-foreground">@{user.user_name}</p>
              )}
              {user.id && (
                <p className="text-xs text-muted-foreground font-mono">
                  ID: {user.id}
                </p>
              )}
            </div>
          </div>
        </CardContent>
      </Card>

      {/* User Details Grid */}
      <div className="grid gap-6 md:grid-cols-2">
        {/* Personal Information */}
        <Card className="border-primary/20">
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <User className="h-5 w-5" />
              Personal Information
            </CardTitle>
            <CardDescription>Basic user details</CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            {user.name && (
              <>
                {user.name.given_name && (
                  <div>
                    <p className="text-sm font-medium text-muted-foreground">
                      First Name
                    </p>
                    <p className="text-lg">{user.name.given_name}</p>
                  </div>
                )}
                {user.name.family_name && (
                  <>
                    <Separator />
                    <div>
                      <p className="text-sm font-medium text-muted-foreground">
                        Last Name
                      </p>
                      <p className="text-lg">{user.name.family_name}</p>
                    </div>
                  </>
                )}
              </>
            )}
            {user.external_id && (
              <>
                <Separator />
                <div>
                  <p className="text-sm font-medium text-muted-foreground">
                    External ID
                  </p>
                  <p className="text-sm font-mono">{user.external_id}</p>
                </div>
              </>
            )}
          </CardContent>
        </Card>

        {/* Contact Information */}
        <Card className="border-primary/20">
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <Mail className="h-5 w-5" />
              Contact Information
            </CardTitle>
            <CardDescription>Email addresses</CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            {user.emails && user.emails.length > 0 ? (
              user.emails.map((email, index) => (
                <div key={index}>
                  {index > 0 && <Separator className="mb-4" />}
                  <div className="flex items-center gap-2">
                    <p className="text-sm break-all">{email.value}</p>
                    {email.primary && (
                      <Badge variant="secondary" className="text-xs">
                        Primary
                      </Badge>
                    )}
                  </div>
                  {email.type && (
                    <p className="text-xs text-muted-foreground mt-1">
                      Type: {email.type}
                    </p>
                  )}
                </div>
              ))
            ) : (
              <p className="text-sm text-muted-foreground">
                No email addresses available
              </p>
            )}
          </CardContent>
        </Card>

        {/* Roles */}
        {user.roles && user.roles.length > 0 && (
          <Card className="border-primary/20">
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <Shield className="h-5 w-5" />
                Roles
              </CardTitle>
              <CardDescription>
                User roles and permissions ({user.roles.length})
              </CardDescription>
            </CardHeader>
            <CardContent>
              <div className="flex flex-wrap gap-2">
                {user.roles.map((role, index) => (
                  <Badge key={index} variant="outline">
                    {role.value}
                  </Badge>
                ))}
              </div>
            </CardContent>
          </Card>
        )}

        {/* Groups */}
        {user.groups && user.groups.length > 0 && (
          <Card className="border-primary/20">
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <Users className="h-5 w-5" />
                Groups
              </CardTitle>
              <CardDescription>
                User group memberships ({user.groups.length})
              </CardDescription>
            </CardHeader>
            <CardContent>
              <div className="flex flex-wrap gap-2">
                {user.groups.map((group, index) => (
                  <Badge key={index} variant="outline">
                    {group.display || group.value}
                  </Badge>
                ))}
              </div>
            </CardContent>
          </Card>
        )}

        {/* Entitlements */}
        {user.entitlements && user.entitlements.length > 0 && (
          <Card className="border-primary/20 md:col-span-2">
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <Shield className="h-5 w-5" />
                Entitlements
              </CardTitle>
              <CardDescription>
                User entitlements ({user.entitlements.length})
              </CardDescription>
            </CardHeader>
            <CardContent>
              <div className="flex flex-wrap gap-2">
                {user.entitlements.map((entitlement, index) => (
                  <Badge key={index} variant="secondary">
                    {entitlement.value}
                  </Badge>
                ))}
              </div>
            </CardContent>
          </Card>
        )}
      </div>
    </div>
  );
}

function ProfileSkeleton() {
  return (
    <div className="max-w-4xl mx-auto space-y-6">
      {/* Header Card Skeleton */}
      <Card className="border-primary/20">
        <CardContent className="pt-6">
          <div className="flex flex-col md:flex-row items-center gap-6">
            <Skeleton className="h-24 w-24 rounded-full" />
            <div className="flex-1 space-y-2 text-center md:text-left">
              <Skeleton className="h-8 w-48 mx-auto md:mx-0" />
              <Skeleton className="h-4 w-32 mx-auto md:mx-0" />
              <Skeleton className="h-3 w-64 mx-auto md:mx-0" />
            </div>
          </div>
        </CardContent>
      </Card>

      {/* Cards Skeleton */}
      <div className="grid gap-6 md:grid-cols-2">
        {[1, 2, 3, 4].map((i) => (
          <Card key={i} className="border-primary/20">
            <CardHeader>
              <Skeleton className="h-6 w-48" />
              <Skeleton className="h-4 w-32" />
            </CardHeader>
            <CardContent className="space-y-2">
              <Skeleton className="h-4 w-full" />
              <Skeleton className="h-4 w-3/4" />
            </CardContent>
          </Card>
        ))}
      </div>
    </div>
  );
}

function Profile() {
  return (
    <div className="relative min-h-screen w-screen overflow-hidden flex flex-col">
      {/* Top bar */}
      <header className="z-50 bg-background/80 backdrop-blur-sm border-b">
        <div className="container mx-auto px-4 h-16 flex items-center justify-between">
          <div className="flex items-center gap-4">
            <Button variant="ghost" size="icon" asChild>
              <Link to="/">
                <ArrowLeft className="h-5 w-5" />
              </Link>
            </Button>
            <div className="flex items-center gap-2">
              <img
                src="/logo.svg"
                alt="logo"
                className="h-6 w-6 text-primary border border-primary rounded-sm"
              />
              <span className="font-semibold text-lg">{__APP_NAME__}</span>
            </div>
          </div>
          <ModeToggle />
        </div>
      </header>

      {/* Main content */}
      <main className="flex-1 container mx-auto px-4 py-8">
        <QueryErrorResetBoundary>
          {({ reset }) => (
            <ErrorBoundary
              onReset={reset}
              fallbackRender={({ resetErrorBoundary }) => (
                <div className="max-w-4xl mx-auto">
                  <Card className="border-destructive/50">
                    <CardHeader>
                      <CardTitle className="flex items-center gap-2 text-destructive">
                        <AlertCircle className="h-5 w-5" />
                        Failed to Load Profile
                      </CardTitle>
                      <CardDescription>
                        There was an error loading your profile information.
                        Make sure the backend is running and you're authenticated.
                      </CardDescription>
                    </CardHeader>
                    <CardContent className="flex gap-2">
                      <Button variant="outline" onClick={resetErrorBoundary}>
                        Try Again
                      </Button>
                      <Button variant="outline" asChild>
                        <Link to="/">Go Home</Link>
                      </Button>
                    </CardContent>
                  </Card>
                </div>
              )}
            >
              <Suspense fallback={<ProfileSkeleton />}>
                <ProfileContent />
              </Suspense>
            </ErrorBoundary>
          )}
        </QueryErrorResetBoundary>
      </main>

      {/* Footer */}
      <footer className="pb-8 px-4">
        <div className="text-center text-sm text-muted-foreground">
          <p className="text-xs">Built with ❤️ using apx</p>
        </div>
      </footer>

      {/* Background gradient effect */}
      <div className="absolute inset-0 -z-10 h-full w-full bg-background">
        <div className="absolute h-full w-full bg-[radial-gradient(circle_at_50%_120%,rgba(120,119,198,0.1),rgba(255,255,255,0))]" />
      </div>
    </div>
  );
}