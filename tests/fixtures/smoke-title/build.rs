fn main() {
    // No C runtime: the loader calls the entry point and takes what it
    // returns as the title's exit code (`victory_loader::spawn_main`); a CRT
    // entry would end the process through `exit` instead.
    println!("cargo:rustc-link-arg-bins=/ENTRY:smoke_main");
    println!("cargo:rustc-link-arg-bins=/NODEFAULTLIB");
    println!("cargo:rustc-link-arg-bins=/SUBSYSTEM:CONSOLE");

    // The draws' console shader containers (src/main.rs): GFX10 code in
    // the layout titles ship, written by victory-shader's `synth`.
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    // The triangle's legs are 0.1 in clip space (of 2): 64 by 36 pixels
    // of the 1280x720 target, about 1/800 of it.
    let shaders = [
        ("triangle.vs.dxbc", synth::vertex_id_triangle(0.1)),
        (
            "color.ps.dxbc",
            synth::constant_color([1.0, 0.75, 0.0, 1.0]),
        ),
    ];
    for (name, bytes) in shaders {
        std::fs::write(out.join(name), bytes).expect("writing a shader container");
    }
}

use victory_shader::native::synth;
