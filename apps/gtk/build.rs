//! Compiles the shipped icons — completion kinds, drawing tools, file types — into a GResource
//! linked into the binary.
//!
//! They travel with the executable rather than through hicolor, so a `cargo run` from the source
//! tree finds them exactly as an installed build does.

fn main() {
    glib_build_tools::compile_resources(&["data"], "data/accent.gresource.xml", "accent.gresource");
}
