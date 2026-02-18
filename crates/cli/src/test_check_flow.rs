//! Integration test: init → check → apply addon → check for each addon.
//!
//! Verifies that the core module templates produce valid, type-checkable code
//! at every stage of the project lifecycle.

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::Path;

    use tempfile::TempDir;

    use crate::common::{Assistant, Layout, Template};
    use crate::dev::apply::{Addon, ApplyArgs};
    use crate::dev::check::CheckArgs;
    use crate::init::InitArgs;

    /// Run `apx init` with the given parameters, asserting exit code 0.
    async fn apx_init(app_path: &Path, template: Template, layout: Option<Layout>) {
        let code = crate::init::run(InitArgs {
            app_path: Some(app_path.to_path_buf()),
            app_name: Some("test-app".to_string()),
            template: Some(template),
            profile: Some("default".to_string()),
            assistant: Some(Assistant::Claude),
            layout,
            as_member: None,
        })
        .await;
        assert_eq!(code, 0, "apx init failed (exit code {code})");
    }

    /// Run `apx dev check` on the given path, asserting exit code 0.
    async fn apx_check(app_path: &Path) {
        let code = crate::dev::check::run(CheckArgs {
            app_path: Some(app_path.to_path_buf()),
        })
        .await;
        assert_eq!(code, 0, "apx dev check failed (exit code {code})");
    }

    /// Run `apx dev apply <addon>` on the given path, asserting exit code 0.
    async fn apx_apply(app_path: &Path, addon: Addon) {
        let code = crate::dev::apply::run(ApplyArgs {
            addon,
            app_path: Some(app_path.to_path_buf()),
            yes: true,
        })
        .await;
        assert_eq!(code, 0, "apx dev apply {addon:?} failed (exit code {code})");
    }

    #[tokio::test]
    async fn test_init_and_addon_check_flow() {
        let dir = TempDir::new().unwrap();
        let app_path = dir.path().join("test-app");

        // Step 1: Init as minimal (no UI)
        apx_init(&app_path, Template::Minimal, None).await;
        assert!(
            app_path.join("src/test_app/backend").exists(),
            "backend directory should exist after minimal init"
        );
        assert!(
            !app_path.join("package.json").exists(),
            "package.json should NOT exist for minimal template"
        );
        // ty-only check (no tsc, no route tree)
        apx_check(&app_path).await;

        // Step 2: Re-init as essential (with UI + sidebar layout, which installs
        // the required shadcn components like sidebar, avatar, card, etc.)
        apx_init(&app_path, Template::Essential, Some(Layout::Sidebar)).await;
        assert!(
            app_path.join("package.json").exists(),
            "package.json should exist for essential template"
        );
        // Full check: tsc + ty
        apx_check(&app_path).await;

        // Step 3: Apply each backend addon, checking after each.
        // Sidebar is already applied via the layout above, so we only test backend addons.
        // Addon configs are validated during lifespan (not import), so no env vars needed.
        for addon in [
            Addon::Sql,
            Addon::Genie,
            Addon::ServingEndpoint,
            Addon::Lakebase,
        ] {
            apx_apply(&app_path, addon).await;
            apx_check(&app_path).await;
        }
    }
}
