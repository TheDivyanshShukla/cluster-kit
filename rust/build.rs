// Embeds the console (../console/build, a static SvelteKit SPA, decision D8) into the binary.
// The build output is committed, so building ck needs no Bun, Node or internet. If it is missing, the
// generated table is empty and the dashboard falls back to the old dash.html.
use std::path::{Path, PathBuf};
use std::{env, fs};

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../console/build");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut out = String::from("pub static FILES: &[(&str, &str, &[u8])] = &[\n");
    if dir.is_dir() {
        walk(&dir, &dir, &mut out);
    }
    out += "];\n";
    fs::write(PathBuf::from(env::var("OUT_DIR").unwrap()).join("ui_files.rs"), out).unwrap();
}

fn walk(root: &Path, d: &Path, out: &mut String) {
    let mut entries: Vec<_> = fs::read_dir(d).unwrap().flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(root, &p, out);
            continue;
        }
        let rel: Vec<String> = p.strip_prefix(root).unwrap().components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        let mime = match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
            "html" => "text/html; charset=utf-8",
            "js" => "text/javascript; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "json" => "application/json",
            "svg" => "image/svg+xml",
            "png" => "image/png",
            "woff2" => "font/woff2",
            "woff" => "font/woff",
            "txt" => "text/plain; charset=utf-8",
            _ => "application/octet-stream",
        };
        out.push_str(&format!("    ({:?}, {mime:?}, include_bytes!({:?})),\n", format!("/{}", rel.join("/")), p.display().to_string()));
    }
}
