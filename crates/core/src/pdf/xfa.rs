//! A hybrid form's XFA datasets (user decision 8): the `datasets` packet of an `/XFA` form beside
//! the AcroForm fields holds their values again for an XFA reader, so a fill writes it too, as
//! pdf.js's `writeXFADataForAcroform` does: the data element a field's full name leads to made to
//! hold its new value, the rest of the packet as it was, byte for byte.

/// What an XFA data element is made to hold: a text, or a `<value>` element each.
pub(super) enum Value<'a> {
    One(&'a str),
    Many(&'a [String]),
}

/// An element of the packet: its name, where its start tag ends and its end tag starts (or, for
/// one closed in its own tag, where that `/>` is), and its children.
struct Element {
    name: String,
    open_end: usize,
    close: usize,
    empty: bool,
    children: Vec<usize>,
    parent: Option<usize>,
}

/// `xml` with the element the field name `path` leads to holding `value`; `None` where no element
/// is found or the packet does not read as XML.
pub(super) fn with_value(xml: &str, path: &str, value: Value) -> Option<String> {
    let tree = elements(xml)?;
    let steps = steps(path);
    let found = search(&tree, 0, &steps)
        .or_else(|| (steps.len() > 1).then(|| search(&tree, 0, &steps[steps.len() - 1..]))?)?;
    let e = &tree[found];
    let body = match value {
        Value::One(text) => escape(text),
        Value::Many(values) => (values.iter())
            .map(|v| format!("<value>{}</value>", escape(v)))
            .collect(),
    };
    Some(match e.empty {
        true => format!(
            "{}>{body}</{}>{}",
            &xml[..e.close],
            e.name,
            &xml[e.open_end..]
        ),
        false => format!("{}{body}{}", &xml[..e.open_end], &xml[e.close..]),
    })
}

/// A field's full name as an XFA path: each part a name and which of its namesakes it is, as
/// in `form1[0].#subform[0].Name[0]` (pdf.js's `parseXFAPath`).
fn steps(path: &str) -> Vec<(&str, usize)> {
    (path.split('.'))
        .map(|part| {
            let indexed = part.strip_suffix(']').and_then(|p| p.rsplit_once('['));
            match indexed.and_then(|(name, n)| Some((name, n.parse().ok()?))) {
                Some((name, n)) if !name.is_empty() => (name, n),
                _ => (part, 0),
            }
        })
        .collect()
}

/// The element `steps` lead to from `at`, as pdf.js's `searchNode` finds it: depth first from
/// `at` itself for the first step's name, then on from there for the rest; a `#` step, an
/// unnamed subform's, passed over but last.
fn search(tree: &[Element], at: usize, steps: &[(&str, usize)]) -> Option<usize> {
    let Some(&(name, n)) = steps.first() else {
        return Some(at);
    };
    if name.starts_with('#') && steps.len() > 1 {
        return search(tree, at, &steps[1..]);
    }
    let mut stack = vec![at];
    while let Some(i) = stack.pop() {
        if tree[i].name == name {
            if n == 0 {
                if let Some(found) = search(tree, i, &steps[1..]) {
                    return Some(found);
                }
            } else {
                let parent = tree[i].parent?;
                let namesakes: Vec<usize> = (tree[parent].children.iter().copied())
                    .filter(|&c| tree[c].name == name)
                    .collect();
                let one = namesakes.get(n).copied().unwrap_or(i);
                return search(tree, one, &steps[1..]);
            }
        }
        stack.extend(tree[i].children.iter().rev());
    }
    None
}

/// The elements of `xml` in document order, the root first; `None` for one whose tags do not
/// nest.
fn elements(xml: &str) -> Option<Vec<Element>> {
    let bytes = xml.as_bytes();
    let (mut tree, mut open): (Vec<Element>, Vec<usize>) = (Vec::new(), Vec::new());
    let mut at = 0;
    while let Some(lt) = memchr::memchr(b'<', &bytes[at..]).map(|i| at + i) {
        let rest = &xml[lt..];
        let skip = [
            ("<!--", "-->"),
            ("<![CDATA[", "]]>"),
            ("<?", "?>"),
            ("<!", ">"),
        ]
        .iter()
        .find(|(start, _)| rest.starts_with(start));
        if let Some((_, end)) = skip {
            at = lt + rest.find(end)? + end.len();
            continue;
        }
        let gt = lt + tag_end(&bytes[lt..])?;
        if let Some(name) = rest.strip_prefix("</") {
            let name = name[..gt - lt - 2].trim();
            let i = open.pop()?;
            if tree[i].name != name {
                return None;
            }
            tree[i].close = lt;
        } else {
            let inner = &xml[lt + 1..gt];
            let empty = inner.ends_with('/');
            let name = inner.trim_end_matches('/');
            let name = name.split(|c: char| c.is_whitespace()).next()?.to_string();
            let parent = open.last().copied();
            let i = tree.len();
            tree.push(Element {
                name,
                open_end: gt + 1,
                close: gt - 1,
                empty,
                children: Vec::new(),
                parent,
            });
            if let Some(p) = parent {
                tree[p].children.push(i);
            }
            if !empty {
                open.push(i);
            }
        }
        at = gt + 1;
    }
    (open.is_empty() && !tree.is_empty()).then_some(tree)
}

/// Where the tag starting `tag` ends, its `>`, past what its attributes quote.
fn tag_end(tag: &[u8]) -> Option<usize> {
    let mut quote = None;
    for (i, &b) in tag.iter().enumerate() {
        match (quote, b) {
            (None, b'"' | b'\'') => quote = Some(b),
            (Some(q), b) if b == q => quote = None,
            (None, b'>') => return Some(i),
            _ => {}
        }
    }
    None
}

fn escape(text: &str) -> String {
    (text.replace('&', "&amp;").replace('<', "&lt;")).replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    const DATASETS: &str = "<xfa:datasets xmlns:xfa=\"http://www.xfa.org/schema/xfa-data/1.0/\">\
        <xfa:data><form1><Name>Old</Name><Agree/><!-- a <note> --><Fruit></Fruit>\
        <Name>Second</Name></form1></xfa:data></xfa:datasets>";

    /// A field's value goes into the element its name leads to, a namesake by its place, an
    /// unnamed subform passed over, one closed in its own tag opened, a list's picks as `<value>`
    /// elements, the rest of the packet untouched.
    #[test]
    fn a_field_s_value_goes_into_its_data_element() {
        let one = |path, v| with_value(DATASETS, path, Value::One(v)).unwrap();
        assert!(one("form1[0].#subform[0].Name[0]", "A&B").contains("<Name>A&amp;B</Name>"));
        let second = one("form1[0].Name[1]", "New");
        assert!(second.contains("<Name>Old</Name>") && second.contains("<Name>New</Name>"));
        let agree = one("form1[0].Agree[0]", "Yes");
        assert!(
            agree.contains("<Agree>Yes</Agree><!-- a <note> -->"),
            "{agree}"
        );
        let picks = ["a".to_string(), "b".to_string()];
        let fruit = with_value(DATASETS, "form1[0].Fruit[0]", Value::Many(&picks)).unwrap();
        assert!(fruit.contains("<Fruit><value>a</value><value>b</value></Fruit>"));
        assert!(
            one("other[0].Name[0]", "x").contains("<Name>x</Name>"),
            "by its last name"
        );
        assert_eq!(with_value(DATASETS, "Nothing[0]", Value::One("x")), None);
        assert_eq!(with_value("<a><b></a>", "b", Value::One("x")), None);
    }
}
