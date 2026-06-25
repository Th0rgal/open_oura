//! Bake the libtorch rpath into the binary when built with `--features torch`,
//! so `oura sessions` finds `libtorch_*.dylib` at runtime without the caller
//! having to export `DYLD_LIBRARY_PATH`. The lib dir is resolved from `LIBTORCH`
//! if set, else from the active `python3`'s torch install (the pip wheel — the
//! only libtorch available on Apple Silicon).

fn main() {
    println!("cargo:rerun-if-env-changed=LIBTORCH");
    println!("cargo:rerun-if-env-changed=LIBTORCH_USE_PYTORCH");
    println!("cargo:rerun-if-env-changed=VIRTUAL_ENV");
    println!("cargo:rerun-if-env-changed=PATH");
    if std::env::var_os("CARGO_FEATURE_TORCH").is_none() {
        return;
    }
    let lib_dir = std::env::var("LIBTORCH")
        .ok()
        .map(|p| format!("{p}/lib"))
        .or_else(torch_lib_from_python);
    if let Some(dir) = lib_dir {
        if std::path::Path::new(&dir).is_dir() {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
        }
    }
}

fn torch_lib_from_python() -> Option<String> {
    let out = std::process::Command::new("python3")
        .args(["-c", "import os,torch;print(os.path.join(os.path.dirname(torch.__file__),'lib'))"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!dir.is_empty()).then_some(dir)
}
