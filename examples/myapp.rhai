// myapp - Example application with multiple dependencies
// Demonstrates a diamond dependency pattern:
//   myapp -> dev-tools -> core-utils
//   myapp -> core-utils (direct dep)

let name = "myapp";
let version = "3.0.0";
let description = "Example application with diamond dependencies";
let installed = false;
let deps = ["dev-tools", "core-utils"];

fn acquire() {
    print("Acquiring myapp...");
}

fn build() {
    print("Building myapp...");
}

fn install() {
    print("Installing myapp to " + PREFIX);
}
