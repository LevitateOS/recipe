// dev-tools - Example package that depends on core-utils
// Demonstrates dependency declaration

let name = "dev-tools";
let version = "2.0.0";
let description = "Developer tools (depends on core-utils)";
let deps = ["core-utils"];

fn acquire() {
    print("Acquiring dev-tools...");
}

fn build() {
    print("Building dev-tools...");
}

fn install() {
    print("Installing dev-tools to " + PREFIX);
}
