//! 使用 `cbindgen` 在构建输出目录中生成 `libzzu.h` C 头文件。

use std::{env, path::PathBuf};

fn main() {
    let directory =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"));
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"));
    let config = cbindgen::Config::from_file(directory.join("cbindgen.toml"))
        .expect("valid cbindgen config");
    cbindgen::Builder::new()
        .with_crate(directory)
        .with_config(config)
        .generate()
        .expect("generate C header")
        .write_to_file(output.join("libzzu.h"));
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=cbindgen.toml");
}
