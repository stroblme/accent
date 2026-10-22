//! Links the system HarfBuzz, the one Pango draws with. In the app `pango.pc` already names it;
//! this is for the crate on its own (`cargo test -p accent-math`) and for the floor `hb.rs`
//! explains.

fn main() {
    if let Err(e) = pkg_config::Config::new()
        .atleast_version("8.3.1")
        .probe("harfbuzz")
    {
        panic!("accent-math needs HarfBuzz 8.3.1 or newer: {e}");
    }
}
