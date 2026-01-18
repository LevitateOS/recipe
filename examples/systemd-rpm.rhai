// systemd - System and Service Manager
// Install from RPM (requires RPM_PATH environment variable)

let name = "systemd";
let version = "257";
let description = "System and Service Manager";

fn acquire() {
    // Copy RPM from configured path
    copy(`${RPM_PATH}/systemd-${version}*.rpm`);
}

fn build() {
    // Already built - nothing to do
}

fn install() {
    rpm_install();
}
