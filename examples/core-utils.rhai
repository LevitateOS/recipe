// core-utils - Example base library (no dependencies)
// Demonstrates a package with no dependencies that others can depend on

let name = "core-utils";
let version = "1.0.0";
let description = "Core utility functions (example base package)";
let deps = [];

fn acquire() {
    // This is an example - no actual download needed
    print("Acquiring core-utils...");
}

fn build() {
    // Nothing to build for this example
    print("Building core-utils...");
}

fn install() {
    // Example only - in a real recipe this would install files
    print("Installing core-utils to " + PREFIX);
}
