//! The slice of the system HarfBuzz this crate calls, declared by hand: the font and its face,
//! the MATH table, GSUB alternates, shaping and glyph outlines. `font.rs` is the only user.
//!
//! The floor is HarfBuzz 8.3.1 (`build.rs`): `hb_ot_math_get_glyph_kerning` follows the MATH
//! specification from there, `hb_ot_layout_lookup_get_glyph_alternates` answers single
//! substitutions from 7.2.0 and the draw API is 7.0.0's.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_uint, c_void};

pub type hb_codepoint_t = u32;
pub type hb_position_t = i32;
pub type hb_tag_t = u32;
pub type hb_bool_t = c_int;
pub type hb_destroy_func_t = Option<unsafe extern "C" fn(*mut c_void)>;

macro_rules! opaque {
    ($($name:ident),*) => {$(
        #[repr(C)]
        pub struct $name {
            _private: [u8; 0],
        }
    )*};
}
opaque!(
    hb_blob_t,
    hb_face_t,
    hb_font_t,
    hb_buffer_t,
    hb_set_t,
    hb_draw_funcs_t,
    hb_draw_state_t
);

pub const fn tag(t: &[u8; 4]) -> hb_tag_t {
    u32::from_be_bytes(*t)
}

pub const HB_MEMORY_MODE_READONLY: c_int = 1;
pub const HB_DIRECTION_LTR: c_int = 4;
pub const HB_DIRECTION_TTB: c_int = 6;
pub const HB_SCRIPT_COMMON: hb_tag_t = tag(b"Zyyy");
pub const HB_OT_TAG_GSUB: hb_tag_t = tag(b"GSUB");
pub const HB_OT_METRICS_TAG_X_HEIGHT: hb_tag_t = tag(b"xhgt");
pub const HB_OT_MATH_GLYPH_PART_FLAG_EXTENDER: c_int = 1;
pub const HB_SET_VALUE_INVALID: hb_codepoint_t = u32::MAX;

#[repr(C)]
#[derive(Default)]
pub struct hb_glyph_extents_t {
    pub x_bearing: hb_position_t,
    pub y_bearing: hb_position_t,
    pub width: hb_position_t,
    pub height: hb_position_t,
}

#[repr(C)]
#[derive(Default)]
pub struct hb_font_extents_t {
    pub ascender: hb_position_t,
    pub descender: hb_position_t,
    line_gap: hb_position_t,
    reserved: [hb_position_t; 9],
}

#[repr(C)]
pub struct hb_glyph_info_t {
    pub codepoint: hb_codepoint_t,
    mask: u32,
    cluster: u32,
    var1: u32,
    var2: u32,
}

#[repr(C)]
pub struct hb_glyph_position_t {
    pub x_advance: hb_position_t,
    y_advance: hb_position_t,
    pub x_offset: hb_position_t,
    pub y_offset: hb_position_t,
    var: u32,
}

#[repr(C)]
pub struct hb_feature_t {
    pub tag: hb_tag_t,
    pub value: u32,
    pub start: c_uint,
    pub end: c_uint,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct hb_ot_math_glyph_variant_t {
    pub glyph: hb_codepoint_t,
    pub advance: hb_position_t,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct hb_ot_math_glyph_part_t {
    pub glyph: hb_codepoint_t,
    pub start_connector_length: hb_position_t,
    pub end_connector_length: hb_position_t,
    pub full_advance: hb_position_t,
    pub flags: c_int,
}

pub type hb_draw_move_to_func_t = unsafe extern "C" fn(
    *mut hb_draw_funcs_t,
    *mut c_void,
    *mut hb_draw_state_t,
    f32,
    f32,
    *mut c_void,
);
pub type hb_draw_quadratic_to_func_t = unsafe extern "C" fn(
    *mut hb_draw_funcs_t,
    *mut c_void,
    *mut hb_draw_state_t,
    f32,
    f32,
    f32,
    f32,
    *mut c_void,
);
pub type hb_draw_cubic_to_func_t = unsafe extern "C" fn(
    *mut hb_draw_funcs_t,
    *mut c_void,
    *mut hb_draw_state_t,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    *mut c_void,
);
pub type hb_draw_close_path_func_t =
    unsafe extern "C" fn(*mut hb_draw_funcs_t, *mut c_void, *mut hb_draw_state_t, *mut c_void);

unsafe extern "C" {
    pub fn hb_blob_create(
        data: *const c_char,
        length: c_uint,
        mode: c_int,
        user_data: *mut c_void,
        destroy: hb_destroy_func_t,
    ) -> *mut hb_blob_t;
    pub fn hb_blob_destroy(blob: *mut hb_blob_t);

    pub fn hb_face_create(blob: *mut hb_blob_t, index: c_uint) -> *mut hb_face_t;
    pub fn hb_face_destroy(face: *mut hb_face_t);
    pub fn hb_face_get_upem(face: *const hb_face_t) -> c_uint;

    pub fn hb_font_create(face: *mut hb_face_t) -> *mut hb_font_t;
    pub fn hb_font_destroy(font: *mut hb_font_t);
    pub fn hb_font_get_face(font: *mut hb_font_t) -> *mut hb_face_t;
    pub fn hb_ot_font_set_funcs(font: *mut hb_font_t);
    pub fn hb_font_set_scale(font: *mut hb_font_t, x_scale: c_int, y_scale: c_int);
    pub fn hb_font_get_nominal_glyph(
        font: *mut hb_font_t,
        unicode: hb_codepoint_t,
        glyph: *mut hb_codepoint_t,
    ) -> hb_bool_t;
    pub fn hb_font_get_glyph_h_advance(
        font: *mut hb_font_t,
        glyph: hb_codepoint_t,
    ) -> hb_position_t;
    pub fn hb_font_get_glyph_extents(
        font: *mut hb_font_t,
        glyph: hb_codepoint_t,
        extents: *mut hb_glyph_extents_t,
    ) -> hb_bool_t;
    pub fn hb_font_get_h_extents(
        font: *mut hb_font_t,
        extents: *mut hb_font_extents_t,
    ) -> hb_bool_t;
    pub fn hb_font_draw_glyph(
        font: *mut hb_font_t,
        glyph: hb_codepoint_t,
        dfuncs: *mut hb_draw_funcs_t,
        draw_data: *mut c_void,
    );

    pub fn hb_ot_metrics_get_position(
        font: *mut hb_font_t,
        metrics_tag: hb_tag_t,
        position: *mut hb_position_t,
    ) -> hb_bool_t;

    pub fn hb_ot_math_has_data(face: *mut hb_face_t) -> hb_bool_t;
    pub fn hb_ot_math_get_constant(font: *mut hb_font_t, constant: c_int) -> hb_position_t;
    pub fn hb_ot_math_get_glyph_italics_correction(
        font: *mut hb_font_t,
        glyph: hb_codepoint_t,
    ) -> hb_position_t;
    pub fn hb_ot_math_get_glyph_top_accent_attachment(
        font: *mut hb_font_t,
        glyph: hb_codepoint_t,
    ) -> hb_position_t;
    pub fn hb_ot_math_get_glyph_kerning(
        font: *mut hb_font_t,
        glyph: hb_codepoint_t,
        kern: c_int,
        correction_height: hb_position_t,
    ) -> hb_position_t;
    pub fn hb_ot_math_get_glyph_variants(
        font: *mut hb_font_t,
        glyph: hb_codepoint_t,
        direction: c_int,
        start_offset: c_uint,
        variants_count: *mut c_uint,
        variants: *mut hb_ot_math_glyph_variant_t,
    ) -> c_uint;
    pub fn hb_ot_math_get_min_connector_overlap(
        font: *mut hb_font_t,
        direction: c_int,
    ) -> hb_position_t;
    pub fn hb_ot_math_get_glyph_assembly(
        font: *mut hb_font_t,
        glyph: hb_codepoint_t,
        direction: c_int,
        start_offset: c_uint,
        parts_count: *mut c_uint,
        parts: *mut hb_ot_math_glyph_part_t,
        italics_correction: *mut hb_position_t,
    ) -> c_uint;

    pub fn hb_set_create() -> *mut hb_set_t;
    pub fn hb_set_destroy(set: *mut hb_set_t);
    pub fn hb_set_next(set: *const hb_set_t, codepoint: *mut hb_codepoint_t) -> hb_bool_t;
    pub fn hb_ot_layout_collect_lookups(
        face: *mut hb_face_t,
        table_tag: hb_tag_t,
        scripts: *const hb_tag_t,
        languages: *const hb_tag_t,
        features: *const hb_tag_t,
        lookup_indexes: *mut hb_set_t,
    );
    pub fn hb_ot_layout_lookup_get_glyph_alternates(
        face: *mut hb_face_t,
        lookup_index: c_uint,
        glyph: hb_codepoint_t,
        start_offset: c_uint,
        alternate_count: *mut c_uint,
        alternate_glyphs: *mut hb_codepoint_t,
    ) -> c_uint;

    pub fn hb_buffer_create() -> *mut hb_buffer_t;
    pub fn hb_buffer_destroy(buffer: *mut hb_buffer_t);
    pub fn hb_buffer_add_utf8(
        buffer: *mut hb_buffer_t,
        text: *const c_char,
        text_length: c_int,
        item_offset: c_uint,
        item_length: c_int,
    );
    pub fn hb_buffer_set_direction(buffer: *mut hb_buffer_t, direction: c_int);
    pub fn hb_buffer_set_script(buffer: *mut hb_buffer_t, script: hb_tag_t);
    pub fn hb_buffer_get_glyph_infos(
        buffer: *mut hb_buffer_t,
        length: *mut c_uint,
    ) -> *mut hb_glyph_info_t;
    pub fn hb_buffer_get_glyph_positions(
        buffer: *mut hb_buffer_t,
        length: *mut c_uint,
    ) -> *mut hb_glyph_position_t;
    pub fn hb_shape(
        font: *mut hb_font_t,
        buffer: *mut hb_buffer_t,
        features: *const hb_feature_t,
        num_features: c_uint,
    );

    pub fn hb_draw_funcs_create() -> *mut hb_draw_funcs_t;
    pub fn hb_draw_funcs_destroy(dfuncs: *mut hb_draw_funcs_t);
    pub fn hb_draw_funcs_make_immutable(dfuncs: *mut hb_draw_funcs_t);
    pub fn hb_draw_funcs_set_move_to_func(
        dfuncs: *mut hb_draw_funcs_t,
        func: hb_draw_move_to_func_t,
        user_data: *mut c_void,
        destroy: hb_destroy_func_t,
    );
    pub fn hb_draw_funcs_set_line_to_func(
        dfuncs: *mut hb_draw_funcs_t,
        func: hb_draw_move_to_func_t,
        user_data: *mut c_void,
        destroy: hb_destroy_func_t,
    );
    pub fn hb_draw_funcs_set_quadratic_to_func(
        dfuncs: *mut hb_draw_funcs_t,
        func: hb_draw_quadratic_to_func_t,
        user_data: *mut c_void,
        destroy: hb_destroy_func_t,
    );
    pub fn hb_draw_funcs_set_cubic_to_func(
        dfuncs: *mut hb_draw_funcs_t,
        func: hb_draw_cubic_to_func_t,
        user_data: *mut c_void,
        destroy: hb_destroy_func_t,
    );
    pub fn hb_draw_funcs_set_close_path_func(
        dfuncs: *mut hb_draw_funcs_t,
        func: hb_draw_close_path_func_t,
        user_data: *mut c_void,
        destroy: hb_destroy_func_t,
    );
}
