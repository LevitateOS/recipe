//! Rhai-based recipe engine
//!
//! Provides the execution environment for recipe scripts.

use anyhow::{Context, Result};
use rhai::{Engine, Scope, EvalAltResult, module_resolvers::FileModuleResolver};
use sha2::{Sha256, Digest};
use std::cell::RefCell;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

thread_local! {
    /// Current execution context for helper functions
    static CONTEXT: RefCell<Option<ExecutionContext>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct ExecutionContext {
    prefix: PathBuf,
    build_dir: PathBuf,
    current_dir: PathBuf,
    last_downloaded: Option<PathBuf>,
}

/// Recipe execution engine
pub struct RecipeEngine {
    engine: Engine,
    prefix: PathBuf,
    build_dir: PathBuf,
    recipes_path: Option<PathBuf>,
}

impl RecipeEngine {
    /// Create a new recipe engine
    pub fn new(prefix: PathBuf, build_dir: PathBuf) -> Self {
        let mut engine = Engine::new();

        // Acquire helpers
        engine.register_fn("download", download);
        engine.register_fn("copy", copy_files);
        engine.register_fn("verify_sha256", verify_sha256);

        // Build helpers
        engine.register_fn("extract", extract);
        engine.register_fn("cd", change_dir);
        engine.register_fn("run", run_cmd);

        // Install helpers
        engine.register_fn("install_bin", install_bin);
        engine.register_fn("install_lib", install_lib);
        engine.register_fn("install_man", install_man);
        engine.register_fn("rpm_install", rpm_install);

        Self {
            engine,
            prefix,
            build_dir,
            recipes_path: None,
        }
    }

    /// Set the recipes path for module resolution
    pub fn with_recipes_path(mut self, path: PathBuf) -> Self {
        let mut resolver = FileModuleResolver::new();
        resolver.set_base_path(&path);
        self.engine.set_module_resolver(resolver);
        self.recipes_path = Some(path);
        self
    }

    /// Execute a recipe script
    pub fn execute(&self, recipe_path: &Path) -> Result<()> {
        let script = std::fs::read_to_string(recipe_path)
            .with_context(|| format!("Failed to read recipe: {}", recipe_path.display()))?;

        // Set up execution context
        let ctx = ExecutionContext {
            prefix: self.prefix.clone(),
            build_dir: self.build_dir.clone(),
            current_dir: self.build_dir.clone(),
            last_downloaded: None,
        };
        CONTEXT.with(|c| *c.borrow_mut() = Some(ctx));

        // Create scope with variables
        let mut scope = Scope::new();
        scope.push_constant("PREFIX", self.prefix.to_string_lossy().to_string());
        scope.push_constant("BUILD_DIR", self.build_dir.to_string_lossy().to_string());
        scope.push_constant("ARCH", std::env::consts::ARCH);
        scope.push_constant("NPROC", num_cpus::get() as i64);
        scope.push_constant("RPM_PATH", std::env::var("RPM_PATH").unwrap_or_default());

        // Compile script
        let ast = self.engine.compile(&script)
            .map_err(|e| anyhow::anyhow!("Failed to compile recipe: {}", e))?;

        // Extract package name for logging
        let name = self.engine
            .eval_ast_with_scope::<String>(&mut scope, &ast)
            .ok()
            .or_else(|| {
                // Try to get 'name' variable from script
                let mut test_scope = scope.clone();
                self.engine.run_ast_with_scope(&mut test_scope, &ast).ok()?;
                test_scope.get_value::<String>("name")
            })
            .unwrap_or_else(|| recipe_path.file_stem().unwrap().to_string_lossy().to_string());

        println!("==> Executing recipe: {}", name);

        // Call phases
        println!("  -> acquire");
        self.call_phase(&mut scope, &ast, "acquire")?;

        println!("  -> build");
        self.call_phase(&mut scope, &ast, "build")?;

        println!("  -> install");
        self.call_phase(&mut scope, &ast, "install")?;

        // Clean up context
        CONTEXT.with(|c| *c.borrow_mut() = None);

        println!("==> Done: {}", name);
        Ok(())
    }

    fn call_phase(&self, scope: &mut Scope, ast: &rhai::AST, phase: &str) -> Result<()> {
        // Check if function exists
        let has_func = ast.iter_functions().any(|f| f.name == phase);

        if !has_func {
            // Phase is optional - just skip if not defined
            return Ok(());
        }

        self.engine
            .call_fn::<()>(scope, ast, phase, ())
            .map_err(|e| anyhow::anyhow!("Phase '{}' failed: {}", phase, e))?;

        Ok(())
    }
}

// ============================================================================
// Acquire helpers
// ============================================================================

fn download(url: &str) -> Result<(), Box<EvalAltResult>> {
    CONTEXT.with(|c| {
        let mut ctx = c.borrow_mut();
        let ctx = ctx.as_mut().ok_or("No execution context")?;

        let filename = url.rsplit('/').next().unwrap_or("download");
        let dest = ctx.build_dir.join(filename);

        println!("     downloading {}", url);

        let status = Command::new("curl")
            .args(["-fsSL", "-o", &dest.to_string_lossy(), url])
            .status()
            .map_err(|e| format!("curl failed: {}", e))?;

        if !status.success() {
            return Err(format!("download failed: {}", url).into());
        }

        ctx.last_downloaded = Some(dest);
        Ok(())
    })
}

fn copy_files(pattern: &str) -> Result<(), Box<EvalAltResult>> {
    CONTEXT.with(|c| {
        let mut ctx = c.borrow_mut();
        let ctx = ctx.as_mut().ok_or("No execution context")?;

        println!("     copying {}", pattern);

        // Expand glob pattern
        let matches: Vec<_> = glob::glob(pattern)
            .map_err(|e| format!("invalid pattern: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        if matches.is_empty() {
            return Err(format!("no files match pattern: {}", pattern).into());
        }

        for src in &matches {
            let filename = src.file_name().ok_or("invalid filename")?;
            let dest = ctx.build_dir.join(filename);
            std::fs::copy(src, &dest)
                .map_err(|e| format!("copy failed: {} -> {}: {}", src.display(), dest.display(), e))?;
            ctx.last_downloaded = Some(dest);
        }

        Ok(())
    })
}

fn verify_sha256(expected: &str) -> Result<(), Box<EvalAltResult>> {
    CONTEXT.with(|c| {
        let ctx = c.borrow();
        let ctx = ctx.as_ref().ok_or("No execution context")?;

        let file = ctx.last_downloaded.as_ref()
            .ok_or("No file to verify - call download() or copy() first")?;

        println!("     verifying sha256");

        let mut f = std::fs::File::open(file)
            .map_err(|e| format!("cannot open file: {}", e))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0; 8192];
        loop {
            let n = f.read(&mut buffer).map_err(|e| format!("read error: {}", e))?;
            if n == 0 { break; }
            hasher.update(&buffer[..n]);
        }
        let hash = hex::encode(hasher.finalize());

        if hash != expected.to_lowercase() {
            return Err(format!("sha256 mismatch: expected {}, got {}", expected, hash).into());
        }

        Ok(())
    })
}

// ============================================================================
// Build helpers
// ============================================================================

fn extract(format: &str) -> Result<(), Box<EvalAltResult>> {
    CONTEXT.with(|c| {
        let ctx = c.borrow();
        let ctx = ctx.as_ref().ok_or("No execution context")?;

        let file = ctx.last_downloaded.as_ref()
            .ok_or("No file to extract - call download() or copy() first")?;

        println!("     extracting {}", file.display());

        let status = match format.to_lowercase().as_str() {
            "tar.gz" | "tgz" => {
                Command::new("tar")
                    .args(["xzf", &file.to_string_lossy()])
                    .current_dir(&ctx.build_dir)
                    .status()
            }
            "tar.xz" | "txz" => {
                Command::new("tar")
                    .args(["xJf", &file.to_string_lossy()])
                    .current_dir(&ctx.build_dir)
                    .status()
            }
            "tar.bz2" | "tbz2" => {
                Command::new("tar")
                    .args(["xjf", &file.to_string_lossy()])
                    .current_dir(&ctx.build_dir)
                    .status()
            }
            "zip" => {
                Command::new("unzip")
                    .args(["-q", &file.to_string_lossy()])
                    .current_dir(&ctx.build_dir)
                    .status()
            }
            _ => return Err(format!("unknown archive format: {}", format).into()),
        };

        let status = status.map_err(|e| format!("extract failed: {}", e))?;
        if !status.success() {
            return Err("extraction failed".to_string().into());
        }

        Ok(())
    })
}

fn change_dir(dir: &str) -> Result<(), Box<EvalAltResult>> {
    CONTEXT.with(|c| {
        let mut ctx = c.borrow_mut();
        let ctx = ctx.as_mut().ok_or("No execution context")?;

        let new_dir = if Path::new(dir).is_absolute() {
            PathBuf::from(dir)
        } else {
            ctx.build_dir.join(dir)
        };

        if !new_dir.exists() {
            return Err(format!("directory does not exist: {}", new_dir.display()).into());
        }

        println!("     cd {}", dir);
        ctx.current_dir = new_dir;
        Ok(())
    })
}

fn run_cmd(cmd: &str) -> Result<(), Box<EvalAltResult>> {
    CONTEXT.with(|c| {
        let ctx = c.borrow();
        let ctx = ctx.as_ref().ok_or("No execution context")?;

        println!("     run: {}", cmd);

        let status = Command::new("sh")
            .args(["-c", cmd])
            .current_dir(&ctx.current_dir)
            .env("PREFIX", &ctx.prefix)
            .env("BUILD_DIR", &ctx.build_dir)
            .status()
            .map_err(|e| format!("command failed to start: {}", e))?;

        if !status.success() {
            return Err(format!("command failed with exit code: {:?}", status.code()).into());
        }

        Ok(())
    })
}

// ============================================================================
// Install helpers
// ============================================================================

fn install_bin(pattern: &str) -> Result<(), Box<EvalAltResult>> {
    install_to_dir(pattern, "bin", Some(0o755))
}

fn install_lib(pattern: &str) -> Result<(), Box<EvalAltResult>> {
    install_to_dir(pattern, "lib", Some(0o644))
}

fn install_man(pattern: &str) -> Result<(), Box<EvalAltResult>> {
    // Man pages go to share/man/man{section}/
    CONTEXT.with(|c| {
        let ctx = c.borrow();
        let ctx = ctx.as_ref().ok_or("No execution context")?;

        let full_pattern = ctx.current_dir.join(pattern);
        let matches: Vec<_> = glob::glob(&full_pattern.to_string_lossy())
            .map_err(|e| format!("invalid pattern: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        if matches.is_empty() {
            return Err(format!("no files match pattern: {}", pattern).into());
        }

        for src in matches {
            let filename = src.file_name().ok_or("invalid filename")?;
            let filename_str = filename.to_string_lossy();

            // Determine man section from extension (e.g., rg.1 -> man1)
            let section = filename_str
                .rsplit('.')
                .next()
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(1);

            let man_dir = ctx.prefix.join(format!("share/man/man{}", section));
            std::fs::create_dir_all(&man_dir)
                .map_err(|e| format!("cannot create dir: {}", e))?;

            let dest = man_dir.join(filename);
            println!("     install {} -> {}", src.display(), dest.display());
            std::fs::copy(&src, &dest)
                .map_err(|e| format!("install failed: {}", e))?;
        }

        Ok(())
    })
}

fn install_to_dir(pattern: &str, subdir: &str, mode: Option<u32>) -> Result<(), Box<EvalAltResult>> {
    CONTEXT.with(|c| {
        let ctx = c.borrow();
        let ctx = ctx.as_ref().ok_or("No execution context")?;

        let full_pattern = ctx.current_dir.join(pattern);
        let matches: Vec<_> = glob::glob(&full_pattern.to_string_lossy())
            .map_err(|e| format!("invalid pattern: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        if matches.is_empty() {
            return Err(format!("no files match pattern: {}", pattern).into());
        }

        let dest_dir = ctx.prefix.join(subdir);
        std::fs::create_dir_all(&dest_dir)
            .map_err(|e| format!("cannot create dir: {}", e))?;

        for src in matches {
            let filename = src.file_name().ok_or("invalid filename")?;
            let dest = dest_dir.join(filename);
            println!("     install {} -> {}", src.display(), dest.display());
            std::fs::copy(&src, &dest)
                .map_err(|e| format!("install failed: {}", e))?;

            #[cfg(unix)]
            if let Some(m) = mode {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(m))
                    .map_err(|e| format!("chmod failed: {}", e))?;
            }
        }

        Ok(())
    })
}

fn rpm_install() -> Result<(), Box<EvalAltResult>> {
    CONTEXT.with(|c| {
        let ctx = c.borrow();
        let ctx = ctx.as_ref().ok_or("No execution context")?;

        // Find RPM files in build_dir
        let pattern = ctx.build_dir.join("*.rpm");
        let matches: Vec<_> = glob::glob(&pattern.to_string_lossy())
            .map_err(|e| format!("invalid pattern: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        if matches.is_empty() {
            return Err("no RPM files found in build directory".to_string().into());
        }

        for rpm in matches {
            println!("     rpm_install {}", rpm.display());

            // Extract RPM contents to prefix using rpm2cpio
            let status = Command::new("sh")
                .args([
                    "-c",
                    &format!(
                        "rpm2cpio '{}' | cpio -idmv -D '{}'",
                        rpm.display(),
                        ctx.prefix.display()
                    ),
                ])
                .current_dir(&ctx.build_dir)
                .status()
                .map_err(|e| format!("rpm2cpio failed: {}", e))?;

            if !status.success() {
                return Err(format!("rpm_install failed for {}", rpm.display()).into());
            }
        }

        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_engine_creation() {
        let prefix = TempDir::new().unwrap();
        let build_dir = TempDir::new().unwrap();
        let engine = RecipeEngine::new(prefix.path().to_path_buf(), build_dir.path().to_path_buf());
        assert!(engine.recipes_path.is_none());
    }

    #[test]
    fn test_empty_recipe() {
        let prefix = TempDir::new().unwrap();
        let build_dir = TempDir::new().unwrap();
        let engine = RecipeEngine::new(prefix.path().to_path_buf(), build_dir.path().to_path_buf());

        let recipe_dir = TempDir::new().unwrap();
        let recipe_path = recipe_dir.path().join("test.rhai");
        std::fs::write(&recipe_path, r#"
            let name = "test";
            let version = "1.0.0";

            fn acquire() {}
            fn build() {}
            fn install() {}
        "#).unwrap();

        let result = engine.execute(&recipe_path);
        assert!(result.is_ok(), "Failed: {:?}", result);
    }
}
