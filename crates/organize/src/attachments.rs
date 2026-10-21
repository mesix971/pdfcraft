//! Document-level file attachments: the catalog's `/Names /EmbeddedFiles` name tree (ISO 32000-2
//! §7.7.4 and §7.11.4). Adding, removing and describing an attachment rewrites the tree flat and
//! sorted, keeping every other entry (and every other name tree) as it was.

use std::collections::HashSet;

use pdfcraft_cos::{Dict, Document, ObjRef, Object, PdfString};

use crate::OrganizeError;

/// More attachments than this in one document is not a file a person made: refuse rather than
/// rewrite a tree that large.
const MAX_ENTRIES: usize = 10_000;
/// A name tree deeper than this is malformed.
const MAX_DEPTH: u8 = 32;
/// The largest file attached in one step (the whole document is held in memory).
pub const MAX_ATTACHMENT_BYTES: usize = 512 * 1024 * 1024;

/// One attachment as the tree lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Attachment {
    /// Its key in the name tree, as text.
    pub name: String,
    /// The file name its file specification gives (`/UF`, else `/F`), or the key.
    pub file: String,
    pub description: String,
    /// The embedded file's size, when its parameters say it.
    pub size: Option<u64>,
}

/// The document's attachments, in the tree's order.
pub fn attachments(doc: &Document) -> Vec<Attachment> {
    entries(doc)
        .into_iter()
        .map(|(key, value)| {
            let fs = doc.resolve(&value).as_dict().cloned().unwrap_or_default();
            let text = |k: &[u8]| fs.get(k).map(|o| doc.resolve(o)).and_then(|o| o.as_string().map(PdfString::to_text));
            let name = PdfString::literal(key).to_text();
            let size =
                fs.get(b"EF").map(|e| doc.resolve(e)).and_then(|e| e.as_dict().and_then(|e| e.get(b"UF").or_else(|| e.get(b"F")).cloned())).and_then(
                    |s| match doc.resolve(&s).as_ref() {
                        Object::Stream(s) => s
                            .dict
                            .get(b"Params")
                            .map(|p| doc.resolve(p))
                            .and_then(|p| p.as_dict().and_then(|p| p.get(b"Size").and_then(Object::as_int)))
                            .and_then(|n| u64::try_from(n).ok()),
                        _ => None,
                    },
                );
            Attachment {
                file: text(b"UF").or_else(|| text(b"F")).unwrap_or_else(|| name.clone()),
                description: text(b"Desc").unwrap_or_default(),
                name,
                size,
            }
        })
        .collect()
}

/// Attach `data` as `file`, described by `description` (empty for none), modified at `date` (a
/// PDF date string, when known). The key is the file name, with " (2)", " (3)"… when one is
/// already taken. Returns the key.
pub fn add_attachment(doc: &mut Document, file: &str, data: &[u8], description: &str, date: Option<&str>) -> Result<String, OrganizeError> {
    let file = file.trim();
    if file.is_empty() || file.contains(['/', '\\']) || file.chars().any(char::is_control) {
        return Err(OrganizeError::Invalid("an attachment needs a file name without folders or control characters".into()));
    }
    if data.len() > MAX_ATTACHMENT_BYTES {
        return Err(OrganizeError::Invalid(format!("an attachment may be at most {} MB", MAX_ATTACHMENT_BYTES / (1024 * 1024))));
    }
    let mut list = entries(doc);
    if list.len() >= MAX_ENTRIES {
        return Err(OrganizeError::Invalid("the document already has too many attachments".into()));
    }
    let taken: HashSet<Vec<u8>> = list.iter().map(|(k, _)| k.clone()).collect();
    let mut key = file.to_owned();
    let mut n = 2u32;
    while taken.contains(&PdfString::text(&key).bytes.clone()) {
        key = format!("{file} ({n})");
        n = n.saturating_add(1);
    }
    let mut params = Dict::new();
    params.set(b"Size".to_vec(), Object::Int(i64::try_from(data.len()).unwrap_or(i64::MAX)));
    if let Some(date) = date {
        params.set(b"ModDate".to_vec(), Object::String(PdfString::literal(date.as_bytes().to_vec())));
    }
    let mut ef = Dict::new();
    ef.set(b"Type".to_vec(), Object::name("EmbeddedFile"));
    ef.set(b"Params".to_vec(), Object::Dict(params));
    let stream = doc.add(Object::Stream(pdfcraft_cos::Stream::flate(ef, data)));
    let mut efd = Dict::new();
    efd.set(b"F".to_vec(), Object::Ref(stream));
    efd.set(b"UF".to_vec(), Object::Ref(stream));
    let mut fs = Dict::new();
    fs.set(b"Type".to_vec(), Object::name("Filespec"));
    fs.set(b"F".to_vec(), Object::String(PdfString::text(file)));
    fs.set(b"UF".to_vec(), Object::String(PdfString::text(file)));
    fs.set(b"EF".to_vec(), Object::Dict(efd));
    if !description.trim().is_empty() {
        fs.set(b"Desc".to_vec(), Object::String(PdfString::text(description.trim())));
    }
    let fs = doc.add(Object::Dict(fs));
    let k = PdfString::text(&key).bytes.clone();
    list.push((k, Object::Ref(fs)));
    write(doc, list)?;
    Ok(key)
}

/// Remove the attachment listed as `name`. Its file stays in the file's earlier revisions until
/// the document is saved in full.
pub fn delete_attachment(doc: &mut Document, name: &str) -> Result<(), OrganizeError> {
    let mut list = entries(doc);
    let before = list.len();
    list.retain(|(k, _)| PdfString::literal(k.clone()).to_text() != name);
    if list.len() == before {
        return Err(OrganizeError::Invalid(format!("there is no attachment named {name:?}")));
    }
    write(doc, list)
}

/// Set the description of the attachment listed as `name` (empty removes it).
pub fn set_attachment_description(doc: &mut Document, name: &str, description: &str) -> Result<(), OrganizeError> {
    let list = entries(doc);
    let value = list
        .iter()
        .find(|(k, _)| PdfString::literal(k.clone()).to_text() == name)
        .map(|(_, v)| v.clone())
        .ok_or_else(|| OrganizeError::Invalid(format!("there is no attachment named {name:?}")))?;
    let set = |fs: &mut Dict| {
        if description.trim().is_empty() {
            fs.remove(b"Desc");
        } else {
            fs.set(b"Desc".to_vec(), Object::String(PdfString::text(description.trim())));
        }
    };
    match value {
        Object::Ref(r) if doc.get(r).as_dict().is_some() => doc.update_dict(r, set)?,
        // A direct file specification lives in the tree's array: rewrite the tree with it changed.
        Object::Dict(mut fs) => {
            set(&mut fs);
            let list = list
                .into_iter()
                .map(|(k, v)| if PdfString::literal(k.clone()).to_text() == name { (k, Object::Dict(fs.clone())) } else { (k, v) })
                .collect();
            write(doc, list)?;
        }
        _ => return Err(OrganizeError::Invalid(format!("the attachment {name:?} has no file specification"))),
    }
    Ok(())
}

/// The tree's entries, each node read once (a malformed tree may share or loop its kids).
fn entries(doc: &Document) -> Vec<(Vec<u8>, Object)> {
    let Some(tree) = names_dict(doc).and_then(|n| n.get(b"EmbeddedFiles").cloned()) else { return Vec::new() };
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    walk(doc, &tree, 0, &mut seen, &mut out);
    out
}

fn walk(doc: &Document, node: &Object, depth: u8, seen: &mut HashSet<ObjRef>, out: &mut Vec<(Vec<u8>, Object)>) {
    if depth > MAX_DEPTH || out.len() >= MAX_ENTRIES {
        return;
    }
    if let Object::Ref(r) = node
        && !seen.insert(*r)
    {
        return;
    }
    let Some(d) = doc.resolve(node).as_dict().cloned() else { return };
    if let Some(names) = d.get(b"Names").map(|n| doc.resolve(n)).and_then(|n| n.as_array().cloned()) {
        for pair in names.chunks(2) {
            if out.len() >= MAX_ENTRIES {
                return;
            }
            if let [k, v] = pair
                && let Some(k) = doc.resolve(k).as_string()
            {
                out.push((k.bytes.clone(), v.clone()));
            }
        }
    }
    for kid in d.get(b"Kids").map(|k| doc.resolve(k)).and_then(|k| k.as_array().cloned()).unwrap_or_default() {
        walk(doc, &kid, depth + 1, seen, out);
    }
}

/// The catalog's `/Names` dictionary, direct or indirect.
fn names_dict(doc: &Document) -> Option<Dict> {
    let catalog = doc.root().and_then(|r| doc.get(r).as_dict().cloned())?;
    catalog.get(b"Names").and_then(|n| doc.resolve(n).as_dict().cloned())
}

/// Replace the tree with `list`, flat and sorted by key (an empty list removes it). Where
/// `/Names` is an indirect dictionary it is updated in place, so its other trees stay.
fn write(doc: &mut Document, mut list: Vec<(Vec<u8>, Object)>) -> Result<(), OrganizeError> {
    list.sort_by(|a, b| a.0.cmp(&b.0));
    let tree = (!list.is_empty()).then(|| {
        let mut names = Vec::with_capacity(list.len() * 2);
        for (k, v) in list {
            names.push(Object::String(PdfString::literal(k)));
            names.push(v);
        }
        let mut tree = Dict::new();
        tree.set(b"Names".to_vec(), Object::Array(names));
        doc.add(Object::Dict(tree))
    });
    let set = |n: &mut Dict| match tree {
        Some(t) => n.set(b"EmbeddedFiles".to_vec(), Object::Ref(t)),
        None => {
            n.remove(b"EmbeddedFiles");
        }
    };
    let root = doc.root().ok_or(OrganizeError::NoPageTree)?;
    let names = doc.get(root).as_dict().and_then(|c| c.get(b"Names").cloned());
    match names {
        Some(Object::Ref(r)) if doc.get(r).as_dict().is_some() => doc.update_dict(r, set)?,
        _ => doc.update_dict(root, |c| {
            let mut n = c.get(b"Names").and_then(Object::as_dict).cloned().unwrap_or_default();
            set(&mut n);
            if n.is_empty() {
                c.remove(b"Names");
            } else {
                c.set(b"Names".to_vec(), Object::Dict(n));
            }
        })?,
    }
    Ok(())
}
