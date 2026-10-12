//! Embed completion, drawing and file-type icons in the executable so source-tree launches
//! find the same icons as installed builds.

fn main() {
    glib_build_tools::compile_resources(&["data"], "data/accent.gresource.xml", "accent.gresource");
}
