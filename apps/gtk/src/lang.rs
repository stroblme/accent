//! What the editor knows about a language server's answers.
//!
//! Only the icon table lives here so far: the per-tab wiring lands beside it.

use accent_api::Kind;

/// The icon for a completion kind. The names are the app's own, shipped in the GResource
/// (`data/icons/scalable/actions`) because Adwaita has no glyph for a function, an enum member or
/// a type parameter. Kinds that mean the same thing to a reader share one drawing: a constructor
/// is a method, a property is a field.
// The completion list is the only caller and arrives with the rewrite of `completion.rs`.
#[allow(dead_code)]
pub fn icon_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Text => "lsp-text-symbolic",
        Kind::Method | Kind::Constructor => "lsp-method-symbolic",
        Kind::Function => "lsp-function-symbolic",
        Kind::Field | Kind::Property => "lsp-field-symbolic",
        Kind::Variable => "lsp-variable-symbolic",
        Kind::Class => "lsp-class-symbolic",
        Kind::Interface => "lsp-interface-symbolic",
        Kind::Module => "lsp-module-symbolic",
        Kind::Enum => "lsp-enum-symbolic",
        Kind::EnumMember => "lsp-enum-member-symbolic",
        Kind::Keyword => "lsp-keyword-symbolic",
        Kind::Snippet => "lsp-snippet-symbolic",
        Kind::Constant => "lsp-constant-symbolic",
        Kind::Struct => "lsp-struct-symbolic",
        Kind::TypeParameter => "lsp-type-parameter-symbolic",
        Kind::File => "lsp-file-symbolic",
        Kind::Folder => "lsp-folder-symbolic",
        Kind::Tag => "lsp-tag-symbolic",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every kind names a file that is actually in the GResource directory: a missing icon is a
    /// blank cell in the completion list and nothing on the console.
    #[test]
    fn every_kind_has_a_shipped_icon() {
        let kinds = [
            Kind::Text,
            Kind::Method,
            Kind::Function,
            Kind::Constructor,
            Kind::Field,
            Kind::Variable,
            Kind::Class,
            Kind::Interface,
            Kind::Module,
            Kind::Property,
            Kind::Enum,
            Kind::Keyword,
            Kind::Snippet,
            Kind::File,
            Kind::Folder,
            Kind::EnumMember,
            Kind::Constant,
            Kind::Struct,
            Kind::TypeParameter,
            Kind::Tag,
        ];
        for kind in kinds {
            let path = format!(
                "{}/data/icons/scalable/actions/{}.svg",
                env!("CARGO_MANIFEST_DIR"),
                icon_name(kind)
            );
            assert!(std::path::Path::new(&path).exists(), "{path}");
        }
    }
}
