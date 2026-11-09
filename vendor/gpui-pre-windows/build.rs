fn main() {
    println!("cargo:rerun-if-changed=src/shaders.hlsl");
    // Shaders are compiled at runtime on Windows with D3DCompile
    // (see directx_renderer.rs). This lets the crate cross-compile
    // from Linux without fxc.exe from the Windows SDK.
}
