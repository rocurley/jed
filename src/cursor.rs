use crate::{
    jq::jv::{JVArray, JVObject, JV},
    lines::{Line, LineContent},
};
use std::{cmp::Ordering, collections::HashSet, fmt, rc::Rc};
use tui::text::Spans;

// Requirements:
// * Produce the current line
// * Step forward
// * (Optionally, for searching): Step backwards
// * Can be "dehydrated" into something hashable for storing folds (other metadata?)

#[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Clone, Copy)]
enum FocusPosition {
    Start,
    Value,
    End,
}

impl FocusPosition {
    pub fn starting(json: &JV) -> Self {
        match json {
            JV::Array(_) | JV::Object(_) => FocusPosition::Start,
            _ => FocusPosition::Value,
        }
    }
    pub fn ending(json: &JV) -> Self {
        match json {
            JV::Array(_) | JV::Object(_) => FocusPosition::End,
            _ => FocusPosition::Value,
        }
    }
}

pub enum CursorFrame {
    Array {
        index: usize,
        json: JVArray,
    },
    Object {
        index: usize,
        key: String,
        json: JVObject,
        iterator: Box<dyn ExactSizeIterator<Item = (String, JV)>>,
    },
}

impl fmt::Debug for CursorFrame {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            CursorFrame::Array { index, json } => fmt
                .debug_struct("Array")
                .field("index", index)
                .field("json", json)
                .finish(),
            CursorFrame::Object {
                index,
                key,
                json,
                iterator: _,
            } => fmt
                .debug_struct("Object")
                .field("index", index)
                .field("key", key)
                .field("json", json)
                .finish(),
        }
    }
}

impl PartialEq for CursorFrame {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                CursorFrame::Array { index, json },
                CursorFrame::Array {
                    index: other_index,
                    json: other_json,
                },
            ) => (index == other_index && json == other_json),
            (
                CursorFrame::Object {
                    index, key, json, ..
                },
                CursorFrame::Object {
                    index: other_index,
                    key: other_key,
                    json: other_json,
                    ..
                },
            ) => (index == other_index && json == other_json && key == other_key),
            _ => false,
        }
    }
}
impl Eq for CursorFrame {}

fn open_container(json: JV) -> (Option<CursorFrame>, JV, FocusPosition) {
    match json {
        JV::Array(arr) => {
            let mut iterator = Box::new(arr.clone().into_iter());
            match iterator.next() {
                None => (None, arr.into(), FocusPosition::End),
                Some(child) => {
                    let focus_position = FocusPosition::starting(&child);
                    (
                        Some(CursorFrame::Array {
                            index: 0,
                            json: arr,
                        }),
                        child,
                        focus_position,
                    )
                }
            }
        }
        JV::Object(obj) => {
            let mut iterator = Box::new(obj.clone().into_iter());
            match iterator.next() {
                None => (None, obj.into(), FocusPosition::End),
                Some((key, child)) => {
                    let focus_position = FocusPosition::starting(&child);
                    (
                        Some(CursorFrame::Object {
                            index: 0,
                            json: obj,
                            key,
                            iterator,
                        }),
                        child,
                        focus_position,
                    )
                }
            }
        }
        _ => panic!("Can't make a cursor frame from a leaf json"),
    }
}

fn open_container_end(json: JV) -> (Option<CursorFrame>, JV, FocusPosition) {
    match json {
        JV::Array(arr) => {
            if arr.is_empty() {
                (None, arr.into(), FocusPosition::Start)
            } else {
                let index = arr.len() - 1;
                let child = arr.get(index).expect("Array should not be empty here");
                let focus_position = FocusPosition::ending(&child);
                (
                    Some(CursorFrame::Array {
                        index: index as usize,
                        json: arr,
                    }),
                    child,
                    focus_position,
                )
            }
        }
        JV::Object(obj) => {
            let iterator = Box::new(obj.clone().into_iter());
            match iterator.last() {
                None => (None, obj.into(), FocusPosition::Start),
                Some((key, child)) => {
                    let index = obj.len() as usize - 1;
                    let focus_position = FocusPosition::ending(&child);
                    (
                        Some(CursorFrame::Object {
                            index,
                            json: obj,
                            key,
                            iterator: Box::new(std::iter::empty()),
                        }),
                        child,
                        focus_position,
                    )
                }
            }
        }
        _ => panic!("Can't make a cursor frame from a leaf json"),
    }
}

impl CursorFrame {
    pub fn index(&self) -> usize {
        match self {
            CursorFrame::Array { index, .. } => *index as usize,
            CursorFrame::Object { index, .. } => *index as usize,
        }
    }
    fn advance(self) -> (Option<Self>, JV, FocusPosition) {
        use CursorFrame::*;
        match self {
            Array { index, json } => match json.get(index as i32 + 1) {
                None => (None, json.into(), FocusPosition::End),
                Some(child) => {
                    let focus_position = FocusPosition::starting(&child);
                    (
                        Some(Array {
                            index: index + 1,
                            json,
                        }),
                        child,
                        focus_position,
                    )
                }
            },
            Object {
                index,
                json,
                mut iterator,
                ..
            } => match iterator.next() {
                None => (None, json.into(), FocusPosition::End),
                Some((key, child)) => {
                    let focus_position = FocusPosition::starting(&child);
                    (
                        Some(Object {
                            index: index + 1,
                            key,
                            json,
                            iterator,
                        }),
                        child,
                        focus_position,
                    )
                }
            },
        }
    }
    fn regress(self) -> (Option<Self>, JV, FocusPosition) {
        use CursorFrame::*;
        match self {
            Array { index, json } => match index.checked_sub(1) {
                None => (None, json.into(), FocusPosition::Start),
                Some(index) => {
                    let child = json
                        .get(index as i32)
                        .expect("Stepped back and didn't find a child");
                    let focus_position = FocusPosition::ending(&child);
                    (Some(Array { index, json }), child, focus_position)
                }
            },
            Object {
                index,
                json,
                iterator: _,
                ..
            } => match index.checked_sub(1) {
                None => (None, json.into(), FocusPosition::Start),
                Some(index) => {
                    let mut iterator = Box::new(json.clone().into_iter());
                    let (key, child) = iterator
                        .nth(index)
                        .expect("Stepped back and didn't find a child");
                    let focus_position = FocusPosition::ending(&child);
                    (
                        Some(Object {
                            index,
                            key,
                            json,
                            iterator,
                        }),
                        child,
                        focus_position,
                    )
                }
            },
        }
    }
}

#[derive(PartialEq, Eq, Debug)]
pub struct Cursor {
    // Top level jsons of the view
    jsons: Rc<[JV]>,
    // Index locating the json this cursor is focused (somewhere) on
    top_index: usize,
    // Stores the ancestors of the current focus, the index of their focused child, and an iterator
    // that will continue right after that child.
    frames: Vec<CursorFrame>,
    // Currently focused json value
    focus: JV,
    // If the json is an array or object, indicates whether the currently focused line is the
    // opening or closing bracket.
    focus_position: FocusPosition,
}

impl Cursor {
    pub fn new(jsons: Rc<[JV]>) -> Option<Self> {
        let focus = jsons.get(0)?.clone();
        let focus_position = FocusPosition::starting(&focus);
        Some(Cursor {
            jsons,
            top_index: 0,
            frames: Vec::new(),
            focus,
            focus_position,
        })
    }
    pub fn to_path(&self) -> Path {
        Path {
            top_index: self.top_index,
            frames: self.frames.iter().map(CursorFrame::index).collect(),
            focus_position: self.focus_position,
        }
    }
    pub fn from_path(jsons: Rc<[JV]>, path: &Path) -> Self {
        let mut focus = jsons[path.top_index].clone();
        let mut frames = Vec::new();
        for &index in path.frames.iter() {
            match focus {
                JV::Array(arr) => {
                    let json = arr.clone();
                    focus = arr
                        .get(index as i32)
                        .expect("Shape of path does not match shape of jsons");
                    frames.push(CursorFrame::Array { index, json });
                }
                JV::Object(obj) => {
                    let json = obj.clone();
                    let mut iterator = Box::new(obj.clone().into_iter());
                    let (key, new_focus) = iterator
                        .nth(index)
                        .expect("Shape of path does not match shape of jsons");
                    focus = new_focus;
                    frames.push(CursorFrame::Object {
                        index,
                        json,
                        key,
                        iterator,
                    });
                }
                _ => panic!("Shape of path does not match shape of jsons"),
            }
        }
        Cursor {
            jsons,
            top_index: path.top_index,
            frames,
            focus,
            focus_position: path.focus_position,
        }
    }
    pub fn current_line(&self, folds: &HashSet<(usize, Vec<usize>)>) -> Line {
        use FocusPosition::*;
        let content = match (&self.focus, self.focus_position) {
            (JV::Object(_), Start) => LineContent::ObjectStart(0),
            (JV::Object(_), End) => LineContent::ObjectEnd(0),
            (JV::Array(_), Start) => LineContent::ArrayStart(0),
            (JV::Array(_), End) => LineContent::ArrayEnd(0),
            (JV::Null(_), Value) => LineContent::Null,
            (JV::Bool(b), Value) => LineContent::Bool(b.value()),
            (JV::Number(x), Value) => LineContent::Number(x.value()),
            (JV::String(s), Value) => LineContent::String(s.value().clone().into()),
            pair => panic!("Illegal json/focus_position pair: {:?}", pair),
        };
        let key = match self.focus_position {
            FocusPosition::End => None,
            _ => match self.frames.last() {
                None => None,
                Some(CursorFrame::Array { .. }) => None,
                Some(CursorFrame::Object { key, .. }) => Some(key.clone().into()),
            },
        };
        let folded = folds.contains(&self.to_path().strip_position());
        let comma = match self.focus_position {
            FocusPosition::Start => false,
            _ => match self.frames.last() {
                None => false,
                Some(CursorFrame::Array { json, index, .. }) => *index != json.len() as usize - 1,
                Some(CursorFrame::Object { iterator, .. }) => iterator.len() != 0,
            },
        };
        let indent = self.frames.len() as u8;
        Line {
            content,
            key,
            folded,
            comma,
            indent,
        }
    }
    pub fn advance(&mut self, folds: &HashSet<(usize, Vec<usize>)>) -> Option<()> {
        // This gets pretty deep into nested match statements, so an english guide to what's going
        // on here.
        // Cases:
        // * We're focused on an open bracket. Push a new frame and start in on the contents of the
        // container. (open_container)
        // * We're focused on a leaf...
        //   * and we have no parent, so advance the very top level, or roll off the end.
        //   * and we have a parent... (Frame::advance)
        //     * and there are more leaves, so focus on the next leaf.
        //     * and there are no more leaves, so pop the frame, focus on the parent's close bracket
        // * We're focused on a close bracket. Advance the parent as if we were focused on a leaf.
        let is_folded = folds.contains(&self.to_path().strip_position());
        match self.focus_position {
            FocusPosition::Start if !is_folded => {
                let (new_frame, new_focus, new_focus_position) = open_container(self.focus.clone());
                if let Some(new_frame) = new_frame {
                    self.frames.push(new_frame);
                }
                self.focus = new_focus;
                self.focus_position = new_focus_position;
            }
            _ => match self.frames.pop() {
                None => {
                    self.focus = self.jsons.get(self.top_index + 1)?.clone();
                    self.top_index += 1;
                    self.focus_position = FocusPosition::starting(&self.focus);
                }
                Some(frame) => {
                    let (new_frame, new_focus, new_focus_position) = frame.advance();
                    if let Some(new_frame) = new_frame {
                        self.frames.push(new_frame);
                    }
                    self.focus = new_focus;
                    self.focus_position = new_focus_position;
                }
            },
        }
        Some(())
    }
    pub fn regress(&mut self, folds: &HashSet<(usize, Vec<usize>)>) -> Option<()> {
        // Pretty mechanical opposite of advance
        match self.focus_position {
            FocusPosition::End => {
                let (new_frame, new_focus, new_focus_position) =
                    open_container_end(self.focus.clone());
                if let Some(new_frame) = new_frame {
                    self.frames.push(new_frame);
                }
                self.focus = new_focus;
                self.focus_position = new_focus_position;
            }
            FocusPosition::Value | FocusPosition::Start => match self.frames.pop() {
                None => {
                    self.top_index = self.top_index.checked_sub(1)?;
                    self.focus = self.jsons[self.top_index].clone();
                    self.focus_position = FocusPosition::ending(&self.focus);
                }
                Some(frame) => {
                    let (new_frame, new_focus, new_focus_position) = frame.regress();
                    if let Some(new_frame) = new_frame {
                        self.frames.push(new_frame);
                    }
                    self.focus = new_focus;
                    self.focus_position = new_focus_position;
                }
            },
        }
        let is_folded = folds.contains(&self.to_path().strip_position());
        if is_folded {
            self.focus_position = FocusPosition::Start;
        }
        Some(())
    }
    pub fn lines_from(
        mut self,
        folds: &HashSet<(usize, Vec<usize>)>,
    ) -> impl Iterator<Item = Line> + '_ {
        let first_line = self.current_line(folds);
        let rest = std::iter::from_fn(move || {
            self.advance(folds)?;
            Some(self.current_line(folds))
        });
        std::iter::once(first_line).chain(rest)
    }
    pub fn render_lines(
        mut self,
        cursor: Option<&Self>,
        folds: &HashSet<(usize, Vec<usize>)>,
        line_limit: u16,
    ) -> Vec<Spans<'static>> {
        let mut lines = Vec::with_capacity(line_limit as usize);
        lines.push(self.current_line(folds).render(Some(&self) == cursor));
        for _ in 0..line_limit {
            if self.advance(folds).is_none() {
                break;
            }
            lines.push(self.current_line(folds).render(Some(&self) == cursor));
        }
        lines
    }
}

impl Clone for Cursor {
    fn clone(&self) -> Self {
        Cursor::from_path(self.jsons.clone(), &self.to_path())
    }
}

#[derive(PartialEq, Eq, Hash, Debug, Clone)]
pub struct Path {
    top_index: usize,
    frames: Vec<usize>,
    focus_position: FocusPosition,
}
impl Path {
    pub fn strip_position(self) -> (usize, Vec<usize>) {
        let Path {
            top_index,
            frames,
            focus_position: _,
        } = self;
        (top_index, frames)
    }
}

impl PartialOrd for Path {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Path {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.top_index.cmp(&other.top_index) {
            Ordering::Equal => {}
            ordering => return ordering,
        }
        let mut self_frames = self.frames.iter();
        let mut other_frames = other.frames.iter();
        loop {
            match (self_frames.next(), other_frames.next()) {
                (Some(self_frame), Some(other_frame)) => match self_frame.cmp(other_frame) {
                    Ordering::Equal => {}
                    ordering => return ordering,
                },
                (None, Some(_)) => match self.focus_position {
                    FocusPosition::Start => return Ordering::Less,
                    FocusPosition::Value => {
                        panic!("Cannot compare paths that index different jsons")
                    }
                    FocusPosition::End => return Ordering::Greater,
                },
                (Some(_), None) => match other.focus_position {
                    FocusPosition::Start => return Ordering::Greater,
                    FocusPosition::Value => {
                        panic!("Cannot compare paths that index different jsons")
                    }
                    FocusPosition::End => return Ordering::Less,
                },
                (None, None) => return self.focus_position.cmp(&other.focus_position),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Cursor, Path};
    use crate::{
        jq::jv::JV,
        lines::{Line, LineContent},
        testing::{arb_json, json_to_lines},
    };
    use pretty_assertions::assert_eq;
    use proptest::proptest;
    use std::{collections::HashSet, rc::Rc};

    fn strip_container_sizes(lines: &mut [Line]) {
        for line in lines {
            match &mut line.content {
                LineContent::ArrayStart(x)
                | LineContent::ArrayEnd(x)
                | LineContent::ObjectStart(x)
                | LineContent::ObjectEnd(x) => *x = 0,
                _ => {}
            }
        }
    }

    proptest! {
        #[test]
        fn prop_lines(values in proptest::collection::vec(arb_json(), 1..10)) {
            let jsons : Vec<JV> = values.iter().map(|v| v.into()).collect();
            let folds = HashSet::new();
            let mut actual_lines = Vec::new();
            if let Some(mut cursor) = Cursor::new(jsons.into()) {
                actual_lines.push(cursor.current_line(&folds));
                while let Some(()) = cursor.advance(&folds) {
                    actual_lines.push(cursor.current_line(&folds));
                }
            }
            let mut expected_lines = json_to_lines(values.iter());
            strip_container_sizes(&mut expected_lines);
            assert_eq!(actual_lines, expected_lines);
        }
    }
    fn check_path_roundtrip(cursor: &Cursor, jsons: Rc<[JV]>) {
        let path = cursor.to_path();
        let new_cursor = Cursor::from_path(jsons, &path);
        assert_eq!(*cursor, new_cursor);
    }
    proptest! {
        #[test]
        fn prop_path_roundtrip(values in proptest::collection::vec(arb_json(), 1..10)) {
            let jsons : Vec<JV> = values.iter().map(|v| v.into()).collect();
            let jsons : Rc<[JV]> = jsons.into();
            let folds = HashSet::new();
            if let Some(mut cursor) = Cursor::new(jsons.clone()) {
                check_path_roundtrip(&cursor, jsons.clone());
                while let Some(()) = cursor.advance(&folds) {
                    check_path_roundtrip(&cursor, jsons.clone());
                }
            }
        }
    }
    fn check_advance_regress(cursor: &Cursor, folds: &HashSet<(usize, Vec<usize>)>) {
        let mut actual: Cursor = cursor.clone();
        if actual.advance(folds).is_none() {
            return;
        }
        actual.regress(folds).unwrap();
        assert_eq!(actual, *cursor);
    }
    proptest! {
        #[test]
        fn prop_advance_regress(values in proptest::collection::vec(arb_json(), 1..10)) {
            let jsons : Vec<JV> = values.iter().map(|v| v.into()).collect();
            let jsons : Rc<[JV]> = jsons.into();
            let folds = HashSet::new();
            if let Some(mut cursor) = Cursor::new(jsons.clone()) {
                check_advance_regress(&cursor, &folds);
                while let Some(()) = cursor.advance(&folds) {
                    check_advance_regress(&cursor, &folds);
                }
            }
        }
    }
    proptest! {
        #[test]
        fn prop_path_ordering(values in proptest::collection::vec(arb_json(), 1..10)) {
            let jsons : Vec<JV> = values.iter().map(|v| v.into()).collect();
            let jsons : Rc<[JV]> = jsons.into();
            let folds = HashSet::new();
            if let Some(mut cursor) = Cursor::new(jsons) {
                let mut prior_path = cursor.to_path();
                while let Some(()) = cursor.advance(&folds) {
                    let new_path = cursor.to_path();
                    dbg!(&new_path, &prior_path);
                    assert!(new_path > prior_path, "Expected {:?} > {:?}", &new_path, &prior_path);
                    prior_path = new_path;
                }
            }
        }
    }
}
