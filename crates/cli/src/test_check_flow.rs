//! Integration test: init → check → apply addon → check for each addon.
//!
//! Verifies that the core module templates produce valid, type-checkable code
//! at every stage of the project lifecycle.

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::Path;

    use tempfile::TempDir;

    use crate::dev::apply::ApplyArgs;
    use crate::dev::check::CheckArgs;
    use crate::init::InitArgs;

    /// Run `apx init` with the given parameters, asserting exit code 0.
    async fn apx_init(app_path: &Path, addons: Vec<&str>) {
        let code = crate::init::run(InitArgs {
            app_path: Some(app_path.to_path_buf()),
            app_name: Some("test-app".to_string()),
            addons: Some(addons.into_iter().map(String::from).collect()),
            no_addons: false,
            profile: Some("default".to_string()),
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
    async fn apx_apply(app_path: &Path, addon: &str) {
        let code = crate::dev::apply::run(ApplyArgs {
            addon: addon.to_string(),
            app_path: Some(app_path.to_path_buf()),
            yes: true,
        })
        .await;
        assert_eq!(code, 0, "apx dev apply {addon} failed (exit code {code})");
    }

    #[tokio::test]
    async fn test_init_and_addon_check_flow() {
        let dir = TempDir::new().unwrap();
        let app_path = dir.path().join("test-app");

        // Step 1: Init without addons (backend-only)
        apx_init(&app_path, vec!["none"]).await;
        assert!(
            app_path.join("src/test_app/backend").exists(),
            "backend directory should exist after no-addon init"
        );
        assert!(
            !app_path.join("package.json").exists(),
            "package.json should NOT exist for no-addon init"
        );
        // ty-only check (no tsc, no route tree)
        apx_check(&app_path).await;

        // Step 2: Re-init with UI + sidebar + claude addons
        apx_init(&app_path, vec!["ui", "sidebar", "claude"]).await;
        assert!(
            app_path.join("package.json").exists(),
            "package.json should exist for ui-enabled init"
        );
        assert!(
            app_path.join(".claude/skills/apx/SKILL.md").exists(),
            "SKILL.md should exist after claude addon"
        );
        // Full check: tsc + ty
        apx_check(&app_path).await;

        // Step 3: Apply each backend addon, checking after each.
        // Addon configs are validated during lifespan (not import), so no env vars needed.
        for addon in ["sql"] {
            apx_apply(&app_path, addon).await;
            apx_check(&app_path).await;
        }
    }
}
