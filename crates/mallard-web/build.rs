//! Embeds the built UI (`ui/dist`) into the binary: generates one table of
//! `(path, content type, bytes)`. Without a built UI the table is empty and
//! the server shows how to build it, so `cargo build` never needs Node.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "json" | "map" => "application/json",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

fn main() {
    let dist = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("ui/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    println!("cargo:rerun-if-changed=build.rs");

    let mut files = Vec::new();
    walk(&dist, &mut files);
    files.sort();
    for file in &files {
        println!("cargo:rerun-if-changed={}", file.display());
    }

    let mut table = String::from("pub static ASSETS: &[(&str, &str, &[u8])] = &[\n");
    for file in &files {
        let rel = file.strip_prefix(&dist).unwrap().to_string_lossy().replace('\\', "/");
        table.push_str(&format!(
            "    ({:?}, {:?}, include_bytes!({:?})),\n",
            format!("/{rel}"),
            content_type(file),
            file
        ));
    }
    table.push_str("];\n");
    fs::write(PathBuf::from(env::var("OUT_DIR").unwrap()).join("assets.rs"), table).unwrap();
}
