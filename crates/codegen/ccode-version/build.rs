fn main() {
    println!("cargo:rerun-if-env-changed=CCODE_VERSION");
}
