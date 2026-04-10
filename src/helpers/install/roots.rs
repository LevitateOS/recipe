use std::cell::RefCell;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub(crate) struct ExecutionRoots {
    sysroot: PathBuf,
    passthrough_roots: Vec<PathBuf>,
}

impl ExecutionRoots {
    pub(crate) fn new(sysroot: PathBuf, prefix: PathBuf) -> Result<Self, String> {
        normalize_path(&prefix, "prefix")?;
        Ok(Self {
            sysroot: normalize_path(&sysroot, "sysroot")?,
            passthrough_roots: Vec::new(),
        })
    }

    pub(crate) fn with_passthrough_root(mut self, root: PathBuf) -> Result<Self, String> {
        self.passthrough_roots
            .push(normalize_path(&root, "passthrough root")?);
        Ok(self)
    }

    pub(crate) fn sysroot(&self) -> &Path {
        &self.sysroot
    }

    fn owning_root<'a>(&'a self, path: &Path) -> Option<&'a Path> {
        if path_is_within(path, &self.sysroot) {
            return Some(&self.sysroot);
        }

        self.passthrough_roots
            .iter()
            .find(|root| path_is_within(path, root))
            .map(PathBuf::as_path)
    }
}

thread_local! {
    static EXECUTION_ROOTS: RefCell<Vec<ExecutionRoots>> = const { RefCell::new(Vec::new()) };
}

pub(crate) struct ScopedExecutionRoots;

impl ScopedExecutionRoots {
    pub(crate) fn push(roots: ExecutionRoots) -> Self {
        EXECUTION_ROOTS.with(|stack| stack.borrow_mut().push(roots));
        Self
    }
}

impl Drop for ScopedExecutionRoots {
    fn drop(&mut self) {
        EXECUTION_ROOTS.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

pub(crate) fn current_roots() -> Option<ExecutionRoots> {
    EXECUTION_ROOTS.with(|stack| stack.borrow().last().cloned())
}

pub(crate) fn resolve_host_path(path: &str) -> Result<PathBuf, String> {
    let raw = Path::new(path);
    let normalized = normalize_input_path(raw)?;
    if !normalized.is_absolute() {
        return Ok(normalized);
    }

    let Some(roots) = current_roots() else {
        return Ok(normalized);
    };

    if let Some(root) = roots.owning_root(&normalized) {
        validate_existing_ancestor_within_root(&normalized, root)?;
        return Ok(normalized);
    }

    let mapped = join_under_root(roots.sysroot(), &normalized);
    validate_existing_ancestor_within_root(&mapped, roots.sysroot())?;
    Ok(mapped)
}

pub(crate) fn resolve_glob_pattern(pattern: &str) -> Result<String, String> {
    validate_path_components(pattern)?;

    let Some(meta_idx) = first_glob_meta(pattern) else {
        return Ok(resolve_host_path(pattern)?.to_string_lossy().to_string());
    };

    let (prefix, suffix) = pattern.split_at(meta_idx);
    if prefix.is_empty() {
        return Ok(pattern.to_string());
    }

    let mapped = resolve_host_path(prefix)?;
    let mut output = mapped.to_string_lossy().to_string();
    if prefix.ends_with(std::path::MAIN_SEPARATOR) && !output.ends_with(std::path::MAIN_SEPARATOR) {
        output.push(std::path::MAIN_SEPARATOR);
    }
    output.push_str(suffix);
    Ok(output)
}

fn first_glob_meta(pattern: &str) -> Option<usize> {
    pattern
        .char_indices()
        .find(|(_, ch)| matches!(ch, '*' | '?' | '['))
        .map(|(idx, _)| idx)
}

fn normalize_input_path(path: &Path) -> Result<PathBuf, String> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {
                return Err(format!(
                    "path '{}' must not contain '.' components",
                    path.display()
                ));
            }
            Component::ParentDir => {
                return Err(format!(
                    "path '{}' must not contain '..' components",
                    path.display()
                ));
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        return Ok(PathBuf::from("."));
    }

    Ok(normalized)
}

fn normalize_path(path: &Path, label: &str) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!(
            "{label} must be an absolute path: {}",
            path.display()
        ));
    }
    normalize_input_path(path)
}

fn validate_path_components(path: &str) -> Result<(), String> {
    for component in Path::new(path).components() {
        match component {
            Component::CurDir => {
                return Err(format!("path '{}' must not contain '.' components", path));
            }
            Component::ParentDir => {
                return Err(format!("path '{}' must not contain '..' components", path));
            }
            _ => {}
        }
    }
    Ok(())
}

fn path_is_within(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

fn join_under_root(root: &Path, path: &Path) -> PathBuf {
    let mut output = root.to_path_buf();
    for component in path.components() {
        if matches!(component, Component::RootDir | Component::Prefix(_)) {
            continue;
        }
        output.push(component.as_os_str());
    }
    output
}

fn validate_existing_ancestor_within_root(path: &Path, root: &Path) -> Result<(), String> {
    if !path_is_within(path, root) {
        return Err(format!(
            "resolved path '{}' escapes root '{}'",
            path.display(),
            root.display()
        ));
    }

    if !root.exists() {
        return Ok(());
    }

    let root_real = root
        .canonicalize()
        .map_err(|e| format!("failed to resolve root '{}': {}", root.display(), e))?;

    let mut cursor = Some(path);
    while let Some(candidate) = cursor {
        if candidate.exists() {
            let real = candidate.canonicalize().map_err(|e| {
                format!(
                    "failed to resolve existing path '{}': {}",
                    candidate.display(),
                    e
                )
            })?;
            if path_is_within(&real, &root_real) {
                return Ok(());
            }
            return Err(format!(
                "resolved path '{}' escapes root '{}' via existing ancestor '{}'",
                path.display(),
                root.display(),
                candidate.display()
            ));
        }
        cursor = candidate.parent();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_host_path_maps_absolute_target_path_under_sysroot() {
        let sysroot = std::env::temp_dir().join("recipe-roots-test-sysroot");
        let prefix = PathBuf::from("/usr/local");
        let roots = ExecutionRoots::new(sysroot.clone(), prefix).unwrap();
        let _guard = ScopedExecutionRoots::push(roots);

        let resolved = resolve_host_path("/etc/hosts").unwrap();
        assert_eq!(resolved, sysroot.join("etc/hosts"));
    }

    #[test]
    fn resolve_host_path_preserves_passthrough_roots() {
        let sysroot = std::env::temp_dir().join("recipe-roots-test-sysroot-pass");
        let passthrough = std::env::temp_dir().join("recipe-roots-build-pass");
        let roots = ExecutionRoots::new(sysroot, PathBuf::from("/usr/local"))
            .unwrap()
            .with_passthrough_root(passthrough.clone())
            .unwrap();
        let _guard = ScopedExecutionRoots::push(roots);

        let resolved = resolve_host_path(passthrough.join("out/file").to_string_lossy().as_ref());
        assert_eq!(resolved.unwrap(), passthrough.join("out/file"));
    }

    #[test]
    fn resolve_host_path_rejects_parent_traversal() {
        let err = resolve_host_path("/tmp/../escape").expect_err("path traversal must fail");
        assert!(err.contains("must not contain '..'"));
    }

    #[test]
    fn resolve_glob_pattern_maps_absolute_prefix() {
        let sysroot = std::env::temp_dir().join("recipe-roots-test-sysroot-glob");
        let roots = ExecutionRoots::new(sysroot.clone(), PathBuf::from("/usr/local")).unwrap();
        let _guard = ScopedExecutionRoots::push(roots);

        let resolved = resolve_glob_pattern("/usr/bin/*.sh").unwrap();
        assert_eq!(resolved, format!("{}/usr/bin/*.sh", sysroot.display()));
    }

    #[test]
    fn execution_roots_validate_prefix_and_expose_sysroot() {
        let roots =
            ExecutionRoots::new(PathBuf::from("/mnt/slot-b"), PathBuf::from("/usr")).unwrap();
        assert_eq!(roots.sysroot(), Path::new("/mnt/slot-b"));
    }
}
