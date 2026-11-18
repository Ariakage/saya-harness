fn main() {
    let dist_dir = std::path::Path::new("../dist");
    let index = dist_dir.join("index.html");
    if !index.exists() {
        let _ = std::fs::create_dir_all(dist_dir);
        let _ = std::fs::write(
            &index,
            "<!doctype html><html><head><meta charset=\"utf-8\"><title>Soyo</title></head><body><div id=\"root\"></div></body></html>\n",
        );
    }
    tauri_build::build()
}
