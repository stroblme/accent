//! Reading and writing the file format: `<mxfile>` → `<diagram>` → `<mxGraphModel>` → `<root>`
//! → `<mxCell>`s, compressed pages included on the way in.

use crate::Error;
use crate::model::File;

/// Parse a whole file.
pub fn parse(bytes: &[u8]) -> Result<File, Error> {
    let _ = bytes;
    todo!("xml::parse")
}

/// The file as XML, uncompressed.
pub fn write(file: &File) -> String {
    let _ = file;
    todo!("xml::write")
}

/// The MIME type and bytes of a `data:` URI, with or without `;base64` (draw.io leaves it out
/// inside a style, where `;` is the separator). `None` for anything else.
pub fn decode_data_uri(uri: &str) -> Option<(String, Vec<u8>)> {
    let rest = uri.strip_prefix("data:")?;
    let (head, data) = rest.split_once(',')?;
    let mime = head.strip_suffix(";base64").unwrap_or(head);
    Some((mime.to_string(), crate::base64::decode(data)?))
}

#[cfg(test)]
mod tests {
    #[test]
    fn data_uris_decode_with_and_without_the_base64_marker() {
        let a = super::decode_data_uri("data:image/png,Zm9v").unwrap();
        let b = super::decode_data_uri("data:image/png;base64,Zm9v").unwrap();
        assert_eq!(a, ("image/png".to_string(), b"foo".to_vec()));
        assert_eq!(a, b);
        assert!(super::decode_data_uri("https://example.org/a.png").is_none());
    }
}
