mod private_tests {
    use crate::core::executor::{compile_recipe, install, parse_extends};
    use crate::core::runner;
    use crate::helpers;
    use rhai::Engine;
    use std::fs;
    use tempfile::TempDir;

    fn create_engine() -> Engine {
        let mut engine = Engine::new();
        helpers::register_all(&mut engine);
        engine
    }

    #[test]
    fn test_install_minimal_recipe() {
        let dir = TempDir::new().unwrap();
        let build_dir = dir.path().join("build");
        fs::create_dir_all(&build_dir).unwrap();

        let recipe_path = dir.path().join("test.rhai");
        fs::write(
            &recipe_path,
            r#"
	let ctx = #{
	    name: "test",
	    installed: false,
	};

	fn is_installed(ctx) {
	    if !ctx.installed { throw "not installed"; }
	    ctx
	}

	fn acquire(ctx) { ctx }
	fn install(ctx) {
	    ctx.installed = true;
	    ctx
	}

	fn cleanup(ctx, reason) { ctx }
	"#,
        )
        .unwrap();

        let engine = create_engine();
        let result = install(&engine, &build_dir, &recipe_path, &[], true, None);
        assert!(result.is_ok(), "Failed: {:?}", result);

        // Check ctx was persisted
        let content = fs::read_to_string(&recipe_path).unwrap();
        assert!(content.contains("installed: true"));
    }

    #[test]
    fn test_install_can_disable_ctx_persistence() {
        let dir = TempDir::new().unwrap();
        let build_dir = dir.path().join("build");
        fs::create_dir_all(&build_dir).unwrap();

        let recipe_path = dir.path().join("test.rhai");
        fs::write(
            &recipe_path,
            r#"
let ctx = #{
    name: "test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }
fn install(ctx) {
    ctx.installed = true;
    ctx
}

fn cleanup(ctx, reason) { ctx }
"#,
        )
        .unwrap();

        let original = fs::read_to_string(&recipe_path).unwrap();
        let engine = create_engine();
        let result = install(&engine, &build_dir, &recipe_path, &[], false, None);
        assert!(result.is_ok(), "Failed: {:?}", result);

        let after = fs::read_to_string(&recipe_path).unwrap();
        assert_eq!(after, original);
    }

    #[cfg(unix)]
    #[test]
    fn test_persist_ctx_preserves_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let build_dir = dir.path().join("build");
        fs::create_dir_all(&build_dir).unwrap();

        let recipe_path = dir.path().join("test.rhai");
        fs::write(
            &recipe_path,
            r#"
    let ctx = #{
        name: "test",
        installed: false,
    };

    fn is_installed(ctx) {
        if !ctx.installed { throw "not installed"; }
        ctx
    }

    fn acquire(ctx) { ctx }
    fn install(ctx) {
        ctx.installed = true;
        ctx
    }

    fn cleanup(ctx, reason) { ctx }
    "#,
        )
        .unwrap();

        fs::set_permissions(&recipe_path, fs::Permissions::from_mode(0o600)).unwrap();
        let before = fs::metadata(&recipe_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(before, 0o600);

        let engine = create_engine();
        install(&engine, &build_dir, &recipe_path, &[], true, None).unwrap();

        let after = fs::metadata(&recipe_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(after, 0o600);
    }

    #[test]
    fn test_install_already_installed_skips() {
        let dir = TempDir::new().unwrap();
        let build_dir = dir.path().join("build");
        fs::create_dir_all(&build_dir).unwrap();

        let recipe_path = dir.path().join("test.rhai");
        fs::write(
            &recipe_path,
            r#"
let ctx = #{
    name: "test",
};

fn is_installed(ctx) { ctx }
fn acquire(ctx) { throw "should not run"; }
fn install(ctx) { throw "should not run"; }
"#,
        )
        .unwrap();

        let engine = create_engine();
        let result = install(&engine, &build_dir, &recipe_path, &[], true, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_is_check_updates_ctx_used_by_later_phases() {
        let dir = TempDir::new().unwrap();
        let build_dir = dir.path().join("build");
        fs::create_dir_all(&build_dir).unwrap();

        // If `is_acquired(ctx)` passes and updates ctx (e.g. `ctx.source_path`),
        // that updated ctx must flow into `build(ctx)`.
        let recipe_path = dir.path().join("test.rhai");
        fs::write(
            &recipe_path,
            r#"
let ctx = #{
    name: "test",
    source_path: "",
    built: false,
    installed: false,
};

fn is_installed(ctx) { throw "not installed"; }
fn is_built(ctx) { throw "not built"; }

fn is_acquired(ctx) {
    // Simulate "already acquired" detection populating derived state.
    ctx.source_path = "/tmp/source-tree";
    ctx
}

fn build(ctx) {
    if ctx.source_path == "" { throw "missing source_path"; }
    ctx.built = true;
    ctx
}

fn install(ctx) {
    if !ctx.built { throw "not built"; }
    ctx.installed = true;
    ctx
}

fn cleanup(ctx, reason) { ctx }
"#,
        )
        .unwrap();

        let engine = create_engine();
        let result = install(&engine, &build_dir, &recipe_path, &[], true, None);
        assert!(result.is_ok(), "Failed: {:?}", result);
    }

    #[test]
    fn test_has_fn() {
        let engine = Engine::new();
        let ast = engine.compile("fn foo() {} fn bar(x) { x }").unwrap();
        assert!(runner::has_fn(&ast, "foo"));
        assert!(runner::has_fn(&ast, "bar"));
        assert!(!runner::has_fn(&ast, "baz"));
    }

    #[test]
    fn test_parse_extends() {
        assert_eq!(
            parse_extends("//! extends: base.rhai\nlet ctx = #{};"),
            Some("base.rhai".to_string())
        );
        assert_eq!(
            parse_extends("//! extends:  linux-base.rhai \nlet ctx = #{};"),
            Some("linux-base.rhai".to_string())
        );
        assert_eq!(
            parse_extends("// comment\n//! extends: base.rhai\nlet ctx = #{};"),
            Some("base.rhai".to_string())
        );
        assert_eq!(parse_extends("let ctx = #{};"), None);
        assert_eq!(
            parse_extends("\n\n//! extends: base.rhai"),
            Some("base.rhai".to_string())
        );
        // Non-comment line before extends stops parsing
        assert_eq!(parse_extends("let x = 1;\n//! extends: base.rhai"), None);
    }

    #[test]
    fn test_extends_merges_functions() {
        let dir = TempDir::new().unwrap();
        let build_dir = dir.path().join("build");
        fs::create_dir_all(&build_dir).unwrap();

        // Base recipe with acquire + install
        let base_path = dir.path().join("base.rhai");
        fs::write(
            &base_path,
            r#"
let ctx = #{
    name: "base",
    acquired: false,
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) {
    ctx.acquired = true;
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}

fn cleanup(ctx, reason) { ctx }
"#,
        )
        .unwrap();

        // Child recipe that extends base, overrides install
        let child_path = dir.path().join("child.rhai");
        fs::write(
            &child_path,
            r#"//! extends: base.rhai

let ctx = #{
    name: "child",
    acquired: false,
    installed: false,
    child_ran: false,
};

fn install(ctx) {
    ctx.installed = true;
    ctx.child_ran = true;
    ctx
}

fn cleanup(ctx, reason) { ctx }
"#,
        )
        .unwrap();

        let engine = create_engine();
        let result = install(&engine, &build_dir, &child_path, &[], true, None);
        assert!(result.is_ok(), "Failed: {:?}", result);

        let ctx = result.unwrap();
        // Child's install ran (child_ran = true)
        assert!(ctx.get("child_ran").unwrap().as_bool().unwrap());
        // Base's acquire ran (acquired = true)
        assert!(ctx.get("acquired").unwrap().as_bool().unwrap());
        // Name should be "child" (child ctx wins)
        assert_eq!(
            ctx.get("name").unwrap().clone().into_string().unwrap(),
            "child"
        );
    }

    #[test]
    fn test_extends_recursive_rejected() {
        let dir = TempDir::new().unwrap();

        let grandparent = dir.path().join("grandparent.rhai");
        fs::write(
            &grandparent,
            "//! extends: nonexistent.rhai\nlet ctx = #{};",
        )
        .unwrap();

        let child = dir.path().join("child.rhai");
        fs::write(&child, "//! extends: grandparent.rhai\nlet ctx = #{};").unwrap();

        let engine = create_engine();
        let result = compile_recipe(&engine, &child, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_extends_base_not_found() {
        let dir = TempDir::new().unwrap();
        let child = dir.path().join("child.rhai");
        fs::write(&child, "//! extends: nonexistent.rhai\nlet ctx = #{};").unwrap();

        let engine = create_engine();
        let result = compile_recipe(&engine, &child, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_extends_persists_ctx_in_base_when_child_has_no_ctx() {
        let dir = TempDir::new().unwrap();
        let build_dir = dir.path().join("build");
        fs::create_dir_all(&build_dir).unwrap();

        let base_path = dir.path().join("base.rhai");
        fs::write(
            &base_path,
            r#"
let ctx = #{
    name: "base",
    acquired: false,
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) {
    ctx.acquired = true;
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}

fn cleanup(ctx, reason) { ctx }
"#,
        )
        .unwrap();

        // Child extends base but does not declare ctx; this is valid as long as
        // ctx persistence targets the file that actually contains `let ctx = #{...};`.
        let child_path = dir.path().join("child.rhai");
        fs::write(
            &child_path,
            r#"//! extends: base.rhai

fn cleanup(ctx, reason) { ctx }
"#,
        )
        .unwrap();

        let engine = create_engine();
        let result = install(&engine, &build_dir, &child_path, &[], true, None);
        assert!(result.is_ok(), "Failed: {:?}", result);

        // Ensure ctx was persisted into base (acquired/installed should be true).
        let persisted = fs::read_to_string(&base_path).unwrap();
        assert!(persisted.contains("acquired: true"));
        assert!(persisted.contains("installed: true"));
    }
}
