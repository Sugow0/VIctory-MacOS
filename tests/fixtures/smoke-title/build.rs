fn main() {
    // No C runtime: the loader calls the entry point and takes what it
    // returns as the title's exit code (`victory_loader::spawn_main`); a CRT
    // entry would end the process through `exit` instead.
    println!("cargo:rustc-link-arg-bins=/ENTRY:smoke_main");
    println!("cargo:rustc-link-arg-bins=/NODEFAULTLIB");
    println!("cargo:rustc-link-arg-bins=/SUBSYSTEM:CONSOLE");
}
