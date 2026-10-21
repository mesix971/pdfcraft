//! Mixed-object inventory and atomic translation in Edit a PDF. Source indexes are resolved
//! together before mutation. Added content appears once, not again as a paragraph/image.

use std::collections::{BTreeMap, BTreeSet};

use pdfcraft_content::{Matrix, Op, Pieces};
use pdfcraft_cos::{Dict, Document, Object};

use crate::{Content, EditError};

pub const MAX_MOVE_OBJECTS: usize = 1_000;
const MAX_PAGE_OBJECTS: usize = 10_000;
const MAX_COORDINATE: f64 = 1e9;
const MAX_CONTENT_BYTES: usize = 128 * 1024 * 1024;

/// The source inventory of an editable item. Indexes are zero-based within that inventory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObjectKind {
    Added,
    Text,
    Image,
}

impl ObjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Text => "text",
            Self::Image => "image",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectTarget {
    pub kind: ObjectKind,
    pub index: usize,
}

/// An item recognised by Edit a PDF. Geometry is in the page's user space; a Form is one
/// grouped artwork item. Its nested content is kept intact, not selected again separately.
#[derive(Clone, Debug, PartialEq)]
pub struct EditableObject {
    pub target: ObjectTarget,
    pub rect: [f64; 4],
    pub content_type: &'static str,
    pub text: Option<String>,
}

fn valid_rect(rect: [f64; 4]) -> bool {
    rect.iter().all(|v| v.is_finite() && v.abs() <= MAX_COORDINATE) && rect[2] > rect[0] && rect[3] > rect[1]
}

pub fn editable_objects(doc: &Document, page: usize) -> Result<Vec<EditableObject>, EditError> {
    checked_streams(doc, page)?;
    let added: Vec<_> = crate::list_added(doc).into_iter().filter(|a| a.page == page).collect();
    if added.len() > MAX_PAGE_OBJECTS {
        return Err(EditError::Invalid("this page has too many objects to select together".into()));
    }
    let geometry = added_geometry(doc, page)?.1;
    let excluded = added.iter().map(|a| a.obj).collect();
    let mut objects = Vec::new();
    for (index, a) in added.into_iter().enumerate() {
        let (content_type, text) = match &a.content {
            Content::Text(t) => ("text", Some(t.text.clone())),
            Content::Image(_) => ("image", None),
        };
        objects.push(EditableObject {
            target: ObjectTarget { kind: ObjectKind::Added, index },
            rect: geometry.get(index).ok_or_else(|| EditError::Invalid("an added item has no placement".into()))?.rect,
            content_type,
            text,
        });
    }
    for (index, b) in crate::text::selectable_blocks(doc, page, &excluded)? {
        objects.push(EditableObject {
            target: ObjectTarget { kind: ObjectKind::Text, index },
            rect: b.rect,
            content_type: "text",
            text: Some(b.text),
        });
    }
    for (index, image) in crate::images::selectable_images(doc, page, &excluded)? {
        let Some(rect) = image.visible_rect else { continue };
        objects.push(EditableObject {
            target: ObjectTarget { kind: ObjectKind::Image, index },
            rect,
            content_type: if image.is_form { "form" } else { "image" },
            text: None,
        });
    }
    if objects.len() > MAX_PAGE_OBJECTS {
        return Err(EditError::Invalid("this page has too many objects to select together".into()));
    }
    if objects.iter().any(|o| !valid_rect(o.rect)) {
        return Err(EditError::Invalid("an object has invalid geometry".into()));
    }
    Ok(objects)
}

/// Move selected mixed objects by a user-space vector as one atomic operation. All targets
/// refer to the original inventories. A refusal leaves the graph, including overlay, untouched.
pub fn move_objects(doc: &mut Document, page: usize, targets: &[ObjectTarget], offset: [f64; 2]) -> Result<(), EditError> {
    if targets.is_empty() || targets.len() > MAX_MOVE_OBJECTS {
        return Err(EditError::Invalid("select between 1 and 1000 objects to move".into()));
    }
    if !offset.iter().all(|v| v.is_finite() && v.abs() <= MAX_COORDINATE) || offset == [0.0, 0.0] {
        return Err(EditError::Invalid("the move must be finite and non-zero".into()));
    }
    let inventory: BTreeMap<_, _> = editable_objects(doc, page)?.into_iter().map(|o| (o.target, o)).collect();
    let mut unique = BTreeSet::new();
    for target in targets {
        if !unique.insert(*target) {
            return Err(EditError::Invalid("an object was selected more than once".into()));
        }
        let object =
            inventory.get(target).ok_or_else(|| EditError::Invalid("a selected object no longer exists; select the objects again".into()))?;
        let r = object.rect;
        if !valid_rect([r[0] + offset[0], r[1] + offset[1], r[2] + offset[0], r[3] + offset[1]]) {
            return Err(EditError::Invalid("the object move is too large".into()));
        }
    }
    let indexes = |kind| targets.iter().filter(|t| t.kind == kind).map(|t| t.index).collect::<Vec<_>>();
    let mut work = doc.clone();
    crate::text::translate_blocks(&mut work, page, &indexes(ObjectKind::Text), offset)?;
    crate::images::translate_images(&mut work, page, &indexes(ObjectKind::Image), offset)?;
    translate_added(&mut work, page, &indexes(ObjectKind::Added), offset)?;
    // Placement operators add to size/depth/work budgets, and moving paragraphs can change
    // their grouping. Keep the resulting inventory usable; refuse before swapping otherwise.
    editable_objects(&work, page)?;
    *doc = work;
    Ok(())
}

struct AddedGeometry {
    rect: [f64; 4],
    local_rect: [f64; 4],
    origin: Matrix,
    outside: Matrix,
    current: Matrix,
    stream: usize,
    op: usize,
    params: Dict,
}

/// An added item's metadata is in the coordinate space it was written in, which can differ
/// from today's page rotation/crop/UserUnit. Read its original placement, including graphics
/// state from preceding streams. MoveOrigin retains that basis after a byte-preserving move.
fn added_geometry(doc: &Document, page: usize) -> Result<(Pieces, Vec<AddedGeometry>), EditError> {
    let added: Vec<_> = crate::list_added(doc).into_iter().filter(|a| a.page == page).collect();
    if added.is_empty() {
        return Ok((Pieces::default(), Vec::new()));
    }
    let streams = checked_streams(doc, page)?;
    let pieces = Pieces::join(&streams.iter().map(|(_, bytes)| bytes.as_slice()).collect::<Vec<_>>());
    let ops = pieces.parse().ops;
    let owned: BTreeSet<_> = added.iter().map(|a| a.obj).collect();
    let mut geometry = BTreeMap::new();
    let mut first = BTreeMap::new();
    let mut matrix = Matrix::IDENTITY;
    let mut stack = Vec::new();
    for (index, op) in ops.iter().enumerate() {
        let (stream, end) = pieces.pieces_of(op);
        let object = streams.get(stream).and_then(|(r, _)| r.as_ref());
        if object.is_some_and(|r| owned.contains(&r)) {
            if let std::collections::btree_map::Entry::Vacant(entry) = first.entry(stream) {
                if stream != end || !op.is("q") {
                    return Err(EditError::Invalid("an added item has an unsupported placement".into()));
                }
                entry.insert((index, matrix));
            } else if first.get(&stream).is_some_and(|(i, _)| i.checked_add(1) == Some(index)) {
                if stream != end || !op.is("cm") {
                    return Err(EditError::Invalid("an added item has an unsupported placement".into()));
                }
                let current = op.nums::<6>().map(Matrix).ok_or_else(|| EditError::Invalid("an added item has invalid geometry".into()))?;
                let outside = first.get(&stream).map_or(Matrix::IDENTITY, |(_, m)| *m);
                let params = object
                    .and_then(|r| doc.get(r).as_dict().and_then(|d| d.get(b"PCAdded")).map(|p| doc.resolve(p)))
                    .and_then(|p| p.as_dict().cloned())
                    .ok_or_else(|| EditError::Invalid("an added item has no parameters".into()))?;
                let origin = match params.get(b"MoveOrigin") {
                    None => current,
                    Some(o) => doc
                        .resolve(o)
                        .as_array()
                        .and_then(|a| Matrix::from_operands(a))
                        .ok_or_else(|| EditError::Invalid("an added item has invalid geometry".into()))?,
                };
                let raw = params
                    .get(b"Rect")
                    .map(|r| doc.resolve(r))
                    .and_then(|r| r.as_array().cloned())
                    .ok_or_else(|| EditError::Invalid("an added item has no rectangle".into()))?;
                let local_rect = raw
                    .iter()
                    .map(Object::as_f64)
                    .collect::<Option<Vec<_>>>()
                    .and_then(|r| <[f64; 4]>::try_from(r).ok())
                    .ok_or_else(|| EditError::Invalid("an added item has invalid geometry".into()))?;
                if !current.0.iter().chain(origin.0.iter()).chain(outside.0.iter()).all(|v| v.is_finite() && v.abs() <= 1e12)
                    || !valid_rect(local_rect)
                {
                    return Err(EditError::Invalid("an added item has invalid geometry".into()));
                }
                geometry.insert(
                    stream,
                    AddedGeometry { rect: origin.then(&outside).bbox(local_rect), local_rect, origin, outside, current, stream, op: index, params },
                );
            }
        }
        match op.op.as_slice() {
            b"q" => {
                if stack.len() >= 1024 {
                    return Err(EditError::Invalid("the page graphics state is nested too deeply".into()));
                }
                stack.push(matrix);
            }
            b"Q" => matrix = stack.pop().unwrap_or(matrix),
            b"cm" => {
                if let Some(m) = op.nums::<6>() {
                    matrix = Matrix(m).then(&matrix);
                }
            }
            _ => {}
        }
    }
    let geometry: Vec<_> = geometry.into_values().collect();
    if geometry.len() != added.len() {
        return Err(EditError::Invalid("an added item has no placement".into()));
    }
    Ok((pieces, geometry))
}

fn translate_added(doc: &mut Document, page: usize, indexes: &[usize], offset: [f64; 2]) -> Result<(), EditError> {
    if indexes.is_empty() {
        return Ok(());
    }
    let p = pdfcraft_model::pages(doc).into_iter().nth(page).ok_or(EditError::NoSuchPage(page))?;
    let streams = checked_streams(doc, page)?;
    let (pieces, geometry) = added_geometry(doc, page)?;
    let ops = pieces.parse().ops;
    let mut replacements = BTreeMap::new();
    let mut parameters = Vec::new();
    let linear = |inverse: Matrix| {
        let [a, b, c, d, _, _] = inverse.0;
        [a * offset[0] + c * offset[1], b * offset[0] + d * offset[1]]
    };
    for index in indexes {
        let g = geometry.get(*index).ok_or_else(|| EditError::Invalid("a selected added item no longer exists".into()))?;
        let delta = linear(g.origin.then(&g.outside).invert().ok_or_else(|| EditError::Invalid("an added item has no area".into()))?);
        let outer_delta = linear(g.outside.invert().ok_or_else(|| EditError::Invalid("an added item has no area".into()))?);
        let current = g.current.then(&Matrix([1.0, 0.0, 0.0, 1.0, outer_delta[0], outer_delta[1]]));
        let r = g.local_rect;
        let rect = [r[0] + delta[0], r[1] + delta[1], r[2] + delta[0], r[3] + delta[1]];
        if !valid_rect(rect) || !current.0.iter().all(|v| v.is_finite() && v.abs() <= 1e12) {
            return Err(EditError::Invalid("the added item move is too large".into()));
        }
        let mut params = g.params.clone();
        params.set(b"MoveOrigin".to_vec(), Object::Array(g.origin.0.map(Object::Real).to_vec()));
        params.set(b"Rect".to_vec(), Object::Array(rect.map(Object::Real).to_vec()));
        parameters.push((g.stream, params));
        replacements.insert(g.op, Op::new("cm", current.0.map(Object::Real).to_vec()));
    }
    // A placement replaces exactly one operator. Keep its existing following separator,
    // rather than adding a blank line every time the item is moved.
    let edits = replacements
        .into_iter()
        .map(|(i, op)| {
            let original = ops.get(i).ok_or_else(|| EditError::Invalid("an added item placement is missing".into()))?;
            let mut bytes = pdfcraft_content::serialize_ops(&[op]);
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
            }
            Ok((original.span.clone(), bytes))
        })
        .collect::<Result<Vec<_>, EditError>>()?;
    let data = pieces.splice(edits);
    let contents = crate::text::rewritten_contents(doc, &streams, data);
    let refs = match &contents {
        Object::Array(a) => a.clone(),
        one => vec![one.clone()],
    };
    for (stream, params) in parameters {
        let r = refs.get(stream).and_then(Object::as_ref).ok_or_else(|| EditError::Invalid("the added item stream is missing".into()))?;
        doc.update_dict(r, |d| d.set(b"PCAdded".to_vec(), Object::Dict(params)))?;
    }
    doc.update_dict(p.obj, |d| d.set(b"Contents".to_vec(), contents))?;
    Ok(())
}

/// A missing or undecodable preceding stream can hide graphics state that affects every
/// later item. Refuse group editing rather than inventing an inventory from partial data.
fn checked_streams(doc: &Document, page: usize) -> Result<Vec<(Object, Vec<u8>)>, EditError> {
    let p = pdfcraft_model::pages(doc).into_iter().nth(page).ok_or(EditError::NoSuchPage(page))?;
    let contents = match p.dict.get(b"Contents") {
        None => Vec::new(),
        Some(c) => match &*doc.resolve(c) {
            Object::Array(a) => {
                if a.len() > MAX_PAGE_OBJECTS {
                    return Err(EditError::Invalid("this page has too many content streams to select objects together".into()));
                }
                a.clone()
            }
            _ => vec![c.clone()],
        },
    };
    let mut remaining = MAX_CONTENT_BYTES;
    let mut depth = 0usize;
    let mut operations = 0usize;
    let mut streams = Vec::with_capacity(contents.len());
    for object in contents {
        let resolved = doc.resolve(&object);
        let Object::Stream(stream) = &*resolved else {
            return Err(EditError::Invalid("a page content stream is missing".into()));
        };
        let bytes = stream
            .decoded_within(remaining.min(64 * 1024 * 1024))
            .map_err(|_| EditError::Invalid("the page content cannot be decoded within its size limit".into()))?;
        remaining =
            remaining.checked_sub(bytes.len()).ok_or_else(|| EditError::Invalid("the page content is too large for group selection".into()))?;
        let parsed = pdfcraft_content::parse(&bytes);
        operations = operations.saturating_add(parsed.ops.len());
        if operations > 100_000 {
            return Err(EditError::Invalid("this page has too many content operations for group selection".into()));
        }
        for op in parsed.ops {
            if op.is("q") {
                depth = depth.saturating_add(1);
            } else if op.is("Q") {
                depth = depth.saturating_sub(1);
            }
            if depth > 1024 {
                return Err(EditError::Invalid("the page graphics state is nested too deeply".into()));
            }
        }
        streams.push((object, bytes));
    }
    Ok(streams)
}

/// How selected objects line up (Acrobat's Align Objects and Distribute).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alignment {
    Left,
    /// Centred on one vertical line.
    Centre,
    Right,
    Top,
    /// Centred on one horizontal line.
    Middle,
    Bottom,
    /// Equal gaps between neighbours from left to right; the outermost stay put.
    DistributeAcross,
    /// Equal gaps between neighbours from top to bottom; the outermost stay put.
    DistributeDown,
}

/// The user-space move each object needs for `how`, by its place in `rects`.
pub fn alignment_offsets(rects: &[[f64; 4]], how: Alignment) -> Vec<[f64; 2]> {
    let lo = |i: usize| rects.iter().map(|r| r[i]).fold(f64::INFINITY, f64::min);
    let hi = |i: usize| rects.iter().map(|r| r[i]).fold(f64::NEG_INFINITY, f64::max);
    let distribute = |axis: usize| {
        // Neighbours by their leading edge; the gaps share out the room between the first and last.
        let mut order: Vec<usize> = (0..rects.len()).collect();
        order.sort_by(|a, b| rects[*a][axis].total_cmp(&rects[*b][axis]).then(a.cmp(b)));
        let mut out = vec![[0.0, 0.0]; rects.len()];
        let (Some(&first), Some(&last)) = (order.first(), order.last()) else { return out };
        let span = rects[last][axis + 2] - rects[first][axis];
        let sizes: f64 = rects.iter().map(|r| r[axis + 2] - r[axis]).sum();
        let gap = (span - sizes) / (rects.len().max(2) - 1) as f64;
        let mut at = rects[first][axis];
        for i in order {
            let mut d = [0.0, 0.0];
            d[axis] = at - rects[i][axis];
            out[i] = d;
            at += rects[i][axis + 2] - rects[i][axis] + gap;
        }
        out
    };
    match how {
        Alignment::Left => rects.iter().map(|r| [lo(0) - r[0], 0.0]).collect(),
        Alignment::Right => rects.iter().map(|r| [hi(2) - r[2], 0.0]).collect(),
        Alignment::Centre => {
            let c = (lo(0) + hi(2)) / 2.0;
            rects.iter().map(|r| [c - (r[0] + r[2]) / 2.0, 0.0]).collect()
        }
        Alignment::Bottom => rects.iter().map(|r| [0.0, lo(1) - r[1]]).collect(),
        Alignment::Top => rects.iter().map(|r| [0.0, hi(3) - r[3]]).collect(),
        Alignment::Middle => {
            let c = (lo(1) + hi(3)) / 2.0;
            rects.iter().map(|r| [0.0, c - (r[1] + r[3]) / 2.0]).collect()
        }
        Alignment::DistributeAcross => distribute(0),
        Alignment::DistributeDown => distribute(1),
    }
}

/// Line up (or spread out) the selected objects, each moved as [`move_objects`] moves it, as one
/// atomic change. Alignment needs two objects, distribution three. Refused, with nothing
/// changed, when a moved object would not land exactly where it should.
pub fn align_objects(doc: &mut Document, page: usize, targets: &[ObjectTarget], how: Alignment) -> Result<(), EditError> {
    let least = if matches!(how, Alignment::DistributeAcross | Alignment::DistributeDown) { 3 } else { 2 };
    if targets.len() < least || targets.len() > MAX_MOVE_OBJECTS {
        let verb = if least == 3 { "distribute" } else { "align" };
        return Err(EditError::Invalid(format!("select between {least} and 1000 objects to {verb}")));
    }
    let inventory: BTreeMap<_, _> = editable_objects(doc, page)?.into_iter().map(|o| (o.target, o.rect)).collect();
    let mut unique = BTreeSet::new();
    let mut rects = Vec::with_capacity(targets.len());
    for target in targets {
        if !unique.insert(*target) {
            return Err(EditError::Invalid("an object was selected more than once".into()));
        }
        let rect = inventory.get(target).ok_or_else(|| EditError::Invalid("a selected object no longer exists; select the objects again".into()))?;
        rects.push(*rect);
    }
    let offsets = alignment_offsets(&rects, how);
    let mut work = doc.clone();
    for (target, offset) in targets.iter().zip(&offsets) {
        if !offset.iter().all(|v| v.is_finite() && v.abs() <= MAX_COORDINATE) {
            return Err(EditError::Invalid("the objects could not be aligned".into()));
        }
        if offset[0].abs() < 1e-9 && offset[1].abs() < 1e-9 {
            continue;
        }
        let one = [target.index];
        match target.kind {
            ObjectKind::Text => crate::text::translate_blocks(&mut work, page, &one, *offset)?,
            ObjectKind::Image => crate::images::translate_images(&mut work, page, &one, *offset)?,
            ObjectKind::Added => translate_added(&mut work, page, &one, *offset)?,
        }
    }
    // Moving one object must not renumber another: check every one landed where it should.
    let after: BTreeMap<_, _> = editable_objects(&work, page)?.into_iter().map(|o| (o.target, o.rect)).collect();
    for ((target, rect), offset) in targets.iter().zip(&rects).zip(&offsets) {
        let want = [rect[0] + offset[0], rect[1] + offset[1], rect[2] + offset[0], rect[3] + offset[1]];
        let got = after.get(target).ok_or_else(|| EditError::Invalid("the objects could not be aligned in place".into()))?;
        if got.iter().zip(&want).any(|(a, b)| (a - b).abs() > 0.05) {
            return Err(EditError::Invalid("the objects could not be aligned in place".into()));
        }
    }
    *doc = work;
    Ok(())
}

/// Where an added item goes in the page's drawing order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrange {
    /// Drawn last, over everything.
    Front,
    /// One step later.
    Forward,
    /// One step earlier.
    Backward,
    /// Drawn first, under the page's own content.
    Back,
}

/// Move added item `index` (in [`crate::list_added`] order on the page) in the drawing order: its
/// content stream moves in the page's `/Contents`. Refused, with nothing changed, if any added
/// item would then be drawn somewhere else (a stream before it that leaves its transform on).
pub fn arrange_added(doc: &mut Document, page: usize, index: usize, to: Arrange) -> Result<(), EditError> {
    let p = pdfcraft_model::pages(doc).into_iter().nth(page).ok_or(EditError::NoSuchPage(page))?;
    let (_, geometry) = added_geometry(doc, page)?;
    let g = geometry.get(index).ok_or_else(|| EditError::Invalid("the added item no longer exists".into()))?;
    let mut before: Vec<[f64; 4]> = geometry.iter().map(|g| g.rect).collect();
    let mut contents = match p.dict.get(b"Contents") {
        Some(c) => match &*doc.resolve(c) {
            Object::Array(a) => a.clone(),
            _ => vec![c.clone()],
        },
        None => Vec::new(),
    };
    let from = g.stream;
    let last = contents.len().checked_sub(1).ok_or_else(|| EditError::Invalid("the page has no content".into()))?;
    if from > last {
        return Err(EditError::Invalid("the added item stream is missing".into()));
    }
    let to_at = match to {
        Arrange::Front => last,
        Arrange::Back => 0,
        Arrange::Forward => (from + 1).min(last),
        Arrange::Backward => from.saturating_sub(1),
    };
    if to_at == from {
        return Ok(());
    }
    let stream = contents.remove(from);
    contents.insert(to_at, stream);
    let mut work = doc.clone();
    work.update_dict(p.obj, |d| d.set(b"Contents".to_vec(), Object::Array(contents)))?;
    let (_, after) = added_geometry(&work, page)?;
    let mut moved: Vec<[f64; 4]> = after.iter().map(|g| g.rect).collect();
    let order = |a: &[f64; 4], b: &[f64; 4]| a.iter().zip(b).map(|(x, y)| x.total_cmp(y)).find(|o| o.is_ne()).unwrap_or(std::cmp::Ordering::Equal);
    moved.sort_by(order);
    before.sort_by(order);
    if moved.len() != before.len() || moved.iter().zip(&before).any(|(a, b)| a.iter().zip(b).any(|(x, y)| (x - y).abs() > 0.05)) {
        return Err(EditError::Invalid("the item can't be moved in the drawing order without moving it on the page".into()));
    }
    *doc = work;
    Ok(())
}
