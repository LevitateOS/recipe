// RPM helper module
// Import with: import "lib/rpm" as rpm;

/// Acquire an RPM from the configured RPM_PATH
fn rpm_acquire(pattern) {
    let full_pattern = `${RPM_PATH}/${pattern}`;
    copy(full_pattern);
}
