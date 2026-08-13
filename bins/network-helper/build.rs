use libbpf_cargo::SkeletonBuilder;
use std::{env, ffi::OsString, path::PathBuf};

fn main() {
    const SOURCE: &str = "src/bpf/network.bpf.c";
    println!("cargo:rerun-if-changed={SOURCE}");

    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"))
        .join("network.bpf.o");
    let mut builder = SkeletonBuilder::new();
    builder.source(SOURCE).obj(&output);
    if let Some(include) = linux_multiarch_include() {
        builder.clang_args([OsString::from("-I"), include.into_os_string()]);
    }
    builder.build().expect("build CO-RE network collector");
}

fn linux_multiarch_include() -> Option<PathBuf> {
    let host = env::var("HOST").ok()?;
    let architecture = host.split('-').next()?;
    let include = PathBuf::from(format!("/usr/include/{architecture}-linux-gnu"));
    include.join("asm/types.h").is_file().then_some(include)
}
